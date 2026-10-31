// announce-rs — 叫号屏摄像头 OCR 语音播报(Rust 版)
// 链路: 摄像头(ffmpeg管道/simulate) -> ROI 帧差变化检测 -> 触发OCR(tesseract)
//       -> 正则校验 -> 多轮观察确认(非阻塞状态机) -> 去重 -> FIFO队列
//       -> 本地TTS播报 -> 日志留痕。
// 与 Python 版(announce.py)逐行为对齐复刻, 差异仅有:
//   - OCR 用 tesseract 子进程(靶机实测 1.43s/次全对), 不再内置 onnxruntime;
//   - 取流用 ffmpeg 子进程管道(纯 Rust 依赖, 可静态链接);
//   - 兜底第二路放大后不做 CLAHE(纯 Rust 无 OpenCV), 仅 2x 放大重试。
//
// 本文件结构(三轮重构: 原 run() 单函数 428 行 -> Loop 结构体 -> OCR 阶段再拆):
//   run()        启动期一次性动作: 读配置 / 抢单实例锁 / 装信号 / 清 _live / 自检依赖
//   Loop::build  把不变参数与跨轮状态收进结构体
//   Loop::run    主循环骨架: 停止检查 -> tick -> 按 fps 补睡眠 -> 收尾
//   Loop::tick   一轮: 取帧 -> 帧差/静帧 -> 诊断 -> 快速确认 -> (按需)OCR -> 维护 -> 健康心跳
//   Loop::maybe_ocr  只做编排; 其下 ocr_due / save_evidence / recognize_candidates /
//                    ingest_candidates / announce_winners / note_ocr_failure 各司其职。
// 纯判定逻辑抽成自由函数以便单测: settle_ready(稳帧) / number_votes(按号码票决) /
//   drop_shadow_empty(空窗口影子候选)。
mod announcer;
mod capture;
mod cfg;
mod dedupe;
mod detect;
mod logger;
mod ocr;
mod selfcheck;
mod signal;
mod tts;
mod util;

use cfg::{Config, SourceKind};
use chrono::Datelike;
use dedupe::{AgedMap, Expiry};
use image::GrayImage;
use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;
use util::now_secs;

/// 触发存证帧与 Web 预览请求的落地目录(与 deploy 侧脚本约定一致)。
const SAVE_DIR: &str = "_live";

/// 「稳帧再认」的最长等待: 画面持续变化超过本秒数就不再等, 直接认当前帧。
/// 防"摄像头噪点/亮度漂移一直判有变化 -> 静置窗口无限续期 -> 永不 OCR"的静默失效。
const SETTLE_MAX_WAIT_SECS: f64 = 2.0;

/// 「号码闸门」的长时间静默兜底: 连续本秒数没有任何播报、而又有更大的号被
/// 硬上限挡住时, 强制放行一次。防"基线被一次误读带偏 -> 后面全被挡 -> 整天不播"。
/// 取 20s(原 60s): 分档确认上线后, 挡住的只剩"跳幅 > JUMP_HARD_MAX"的极端情况,
/// 20s 足够判定它是真号而不是误读, 又不至于让现场等一分钟。
const GATE_FORCE_SECS: f64 = 20.0;

/// 跳幅分档(2026-10-21 现场要求): 叫号员会过号, 卡死 +2 会漏播真号。
///   ≤JUMP_INSTANT      —— 立即播(和原来一样, 覆盖绝大多数 +1/+2)
///   ≤max_jump          —— 多要 2 个确认量(静帧 tick 或投票轮), 防"过渡帧残影"
///   ≤JUMP_HARD_MAX     —— 多要 4 个确认量, 覆盖大过号
///   >JUMP_HARD_MAX     —— 直接挡, 只留 GATE_FORCE_SECS 兜底(基本只可能是误读)
const JUMP_INSTANT: u32 = 3;
const JUMP_HARD_MAX: u32 = 30;

/// 漏号补播窗口(2026-10-23 现场 GA126 事故): 两个窗口连续叫号时, 新号把旧号
/// 顶到第 2 行, 若 OCR 在旧号的第 1 行任期内没凑齐确认(滚屏动画 + 确认轮耗时),
/// 该号就卡在第 2 行 —— 提前退出不再读第 2 行、单调闸门只放行 v>last,
/// 双重压制导致"叫了号却没播"。本窗口放行 (last-BACKFILL_MAX, last] 且
/// 【当日从未播过】的候选; 已播过的屏上残留由当日去重表(开机从日志 seed)
/// 挡住。K=5 覆盖两窗口紧挨着连续叫号的场景。
const BACKFILL_MAX: u32 = 5;

/// 高亮行增强会把图像放大到 2 倍，并跑 31x31 局部均值。限制原始 ROI 总像素，
/// 防止高度误配时预处理在 tesseract 超时保护之外长时间占满主循环。
const MAX_ENHANCE_PIXELS: u64 = 400_000;

/// ★序列重置(2026-10-26 现场 GA174→GA193 事故): 候选【远小于基线】(差 > BACKFILL_MAX)
/// 时, 若同一号**连续 K 轮稳定出现**, 判定为"新一轮序列开始"(回退/换队列/系统重置
/// 编号), 重置基线为该号并正常播报它 —— 它就是屏幕当前真实叫号。
///
/// 事故复盘: 屏上第 2 行是换号动画的乱码残影, OCR 把它读成 GA193。174→193 只差 19,
/// 落在 +10~+30 的"多确认"档 —— 而残影在屏上稳定驻留了好几秒, 多要的那几票照样
/// 攒够, 于是误读被放行、基线毒化成 193, 之后真号 GA175~178 全被"号小了差>5"挡死。
/// 更糟的是旧逻辑里"小号"这条分支**根本没有出口**: 只能等 20s 静默兜底, 而兜底只放
/// 行"更大的号", 于是当天剩余小号全程哑火。
///
/// 为什么按"秒"计而不是按"轮"计: 现场实测残影的寿命是**墙钟时间**(换号动画结束
/// 就消失), 与 OCR 跑了几轮无关。按轮计遇到主码流下轮次变快(每轮 <0.5s)时,
/// 残影在同样的 3s 寿命里能轻松攒到 5 轮 —— 阈值形同虚设。按秒计才是直接对着
/// "残影能活多久"设闸。
///
/// 计时基准: 取**当前静帧段起点 static_since**(屏上画面不再变化的那一刻)算起,
/// 而不是"第一次看见该号"的墙钟时间 —— 屏在动(换号动画)时根本不计时,
/// 画面一静就重新起算, 所以残影永远攒不满。
///
/// SEQ_RESET_SECS=8.0 的理由 —— 防误读残影误重置:
///   * 残影撑不过真号更替: 换号动画一结束残影就消失, 实测驻留 <5s, 攒不到 8s;
///   * 真回退/新一轮小号会**稳定存在**十几秒以上, 甚至一直挂在屏上, 8s 轻松过;
///   * 代价只是极端回退场景晚播约 8s, 收益是当天不再整段哑火。
const SEQ_RESET_SECS: f64 = 8.0;

/// 序列重置判定(纯函数, 供单测): 该候选是否"远低于基线"。
/// 条件: 有基线、候选数值 < 基线、且差值 > BACKFILL_MAX(补播窗口之外的号)。
fn seq_reset_eligible(has_baseline: bool, last: u32, v: u32) -> bool {
    has_baseline && v < last && last - v > BACKFILL_MAX
}

/// 序列重置计时推进(纯函数, 供单测): 返回 (该号当前静帧段已持续秒数, 是否够钟可重置)。
///
/// 调用方在**每个静帧段内第一次看见该号**时传 since=now(起算); 之后每轮传上次
/// 记下的 since。静帧段一变(屏又动了)调用方必须整表清空 —— 于是"同一静帧段内
/// 连续稳定 8s"是硬约束: 残影跟着换号动画一起消失, 永远攒不满。
/// since 为 None 表示"本静帧段刚加入", 本轮记 0s 不计入。
fn seq_reset_tick(since: Option<f64>, now: f64) -> (f64, bool) {
    match since {
        None => (now, false),
        Some(s) => {
            let held = now - s;
            (s, held >= SEQ_RESET_SECS)
        }
    }
}

/// 跳幅 -> 额外需要的确认量。0 表示"够票即播, 不额外等"。
/// 抽成自由函数便于单测: 这是现场调参的核心决策, 不能让口径藏在方法里。
fn jump_extra_confirm(delta: u32, max_jump: u32) -> u32 {
    if max_jump == 0 {
        return 0; // 闸门关闭
    }
    let instant = JUMP_INSTANT.min(max_jump);
    if delta <= instant {
        0
    } else if delta <= max_jump {
        2
    } else {
        4
    }
}

/// 确认窗口的有效 TTL(秒) —— 取四个下界的最大值。
///
/// - `ttl`: 配置值 `confirm_ttl_sec`;
/// - `3 × 本轮 OCR 耗时`: OCR 一步就要 1.5~10s 时, 固定 4s 的 TTL 会在下一轮
///   到来之前把候选清掉, 确认数永远凑不齐 -> 不播报;
/// - `1.5 × fb_prev` / `1.5 × fb_next`(本轮实际等待的、下一轮将要等待的兜底间隔):
///   ★死锁防线(2026-10-22 现场): 过曝/条纹帧下 tesseract 是 0.1s 就正常退出 +
///   空输出, **不算失败**, 于是 empty_streak 一路涨到 4 → 兜底间隔被拉到 30s,
///   而 TTL 按 3×0.1s 只算出 4s → 下一轮开头 retain 就把候选连计数一起清掉,
///   确认数永远是 1(现场 16:14~16:54 的静默不播就是这个死法)。
///   退避的语义是"少跑几次 OCR", 不该连带惩罚已经确认过的候选。
///
/// 抽成自由函数便于单测: 这条防线一旦退化, 症状是"屏幕有号但永远不播", 极难定位。
fn effective_ttl(ttl: f64, iter_secs: f64, fb_prev: f64, fb_next: f64) -> f64 {
    ttl.max(3.0 * iter_secs)
        .max(1.5 * fb_prev)
        .max(1.5 * fb_next)
}

/// 行距(像素)的解析: 绝对像素优先, 其次按"相对行高"折算, 都没有则 0(退化为单行)。
///
/// 抽成自由函数便于单测。换码流/换分辨率时 `row_pitch` 的绝对像素必须重新标定
/// (2026-10-22 现场就是没标定, 第 2/3 行切到空隙); 用 `row_pitch_ratio` 表达
/// "行距 = 行高 × 比值"可以让它自动跟随分辨率, 免掉每次换算。
fn resolve_row_pitch(row_pitch: u32, ratio: f64, roi_h: usize) -> usize {
    if row_pitch > 0 {
        return row_pitch as usize;
    }
    if ratio > 0.0 && roi_h > 0 {
        return (roi_h as f64 * ratio).round().max(1.0) as usize;
    }
    0
}

/// 绑定本机回环端口作为单实例锁。端口被占 -> 已有实例在跑。
/// 与 Python 版一致: 不设 SO_REUSEADDR(std 默认就不设), 进程死操作系统自动释放。
fn acquire_lock(port: u16) -> Result<TcpListener, String> {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => Ok(l),
        Err(_) => Err(format!(
            "检测到已有另一个 announce 实例在运行(占用 127.0.0.1:{})。\n\
             摄像头是独占资源, 重复启动会让画面错乱、OCR 全部识别失败。\n\
             如需重启, 请先结束旧进程再启动本程序。",
            port
        )),
    }
}

/// 一轮循环的结束方式 —— 决定主循环要不要按 fps 节奏补睡眠。
enum Tick {
    /// 正常跑完一轮, 主循环补足剩余间隔。
    Done,
    /// 本轮已经自行等待(取帧失败 sleep 1s), 或无需等待(ROI 越界跳帧):
    /// 主循环直接进下一轮, 不再补睡眠。
    SkipSleep,
}

/// 稳帧判定(纯函数, 便于单测): 返回 `(ready, by_change)`。
///
/// 语义: `ready` = 画面是否已定型、可以送 OCR(静置窗口到点, 或变化已超最长等待);
/// `by_change` = 本次是否"为这一轮变化"而认 —— 只有它为真才存证帧,
/// 避免把换号动画的过渡帧(残影)存成证据。
///
/// 最长等待兜底是**必须有**的: 若画面"一直在变"(摄像头噪点 / 自动曝光漂移超过
/// changed_ratio), 静置窗口会被每一帧无限续期 -> 永远不 ready -> 服务活着却一句话
/// 不播(最危险的静默软故障)。所以变化持续超过 SETTLE_MAX_WAIT_SECS 就强制放行一次。
fn settle_ready(
    now: f64,
    settle_until: f64,
    change_fired: bool,
    change_start: f64,
) -> (bool, bool) {
    let overdue = !change_fired && change_start > 0.0 && now - change_start >= SETTLE_MAX_WAIT_SECS;
    let ready = now >= settle_until || overdue;
    (ready, ready && !change_fired)
}

/// 运行期诊断用的源描述(仅打印, 无逻辑作用)。
fn src_desc(cfg: &Config) -> String {
    match cfg.source() {
        SourceKind::Simulate => "simulate".to_string(),
        SourceKind::Net(u) => u,
        SourceKind::Device(d) => d,
    }
}

/// 主循环的全部"不变参数 + 跨轮状态"。
///
/// 为什么收进结构体: 原先它们是一个 428 行函数里平铺的 30 来个局部变量, 内嵌 5 层,
/// "哪一段改了哪个变量"只能靠通读(二轮评审 P2-2)。收进来之后, 每个阶段是一个有名字的
/// 方法, 状态归属由字段说明显式表达 —— 行为一行未改, 只是把隐式耦合变成显式。
struct Loop {
    // ———— 启动期算好、循环内只读 ————
    cfg: Config,
    det: cfg::Detect,
    roi: (usize, usize, usize, usize),
    /// 每轮目标间隔(1/fps)。
    interval: f64,
    /// 候选需连续观察到多少轮才确认播报。
    need_stable: u32,
    /// 候选确认窗口(TTL)基准值, 实际用时会按本轮 OCR 耗时自适应放大。
    ttl: f64,
    /// 一次"有变化"之后, 观察期延长到多久(期间持续触发确认 OCR)。
    active_sec: f64,
    /// 兜底 OCR 基础间隔(防帧差漏检的保险丝)。
    fb_sec: f64,
    /// 屏面静止且内容已知时的兜底间隔(放宽, 省 CPU)。
    static_fb_sec: f64,
    /// 静帧快速确认所需的无变化轮数(0 = 关闭该优化)。
    confirm_ticks: u32,
    diag_on: bool,
    diag_every: f64,
    save_frames: bool,
    /// 维护节拍间隔(清理去重表 + 日志轮转检查)。
    maint_every: f64,
    log_max_bytes: u64,
    /// 「健康心跳」阈值(秒)。0 = 关闭。见 cfg.rs 的 health_alert_min。
    health_alert_sec: f64,

    // ———— 组件 ————
    ocr: ocr::Ocr,
    ann: Arc<announcer::Announcer>,
    src: capture::Source,

    // ———— 跨轮状态 ————
    prev_gray: Option<GrayImage>,
    roi_warned: bool,
    /// 已播报去重表 —— 带时间维度, 由"维护节拍"定期清理(见 dedupe.rs)。
    /// 旧实现只 insert 不删除, 号码跨天复用时会被永久判为"已播报"而静默哑掉。
    /// ★键是【号码】而非"号码+窗口": 现场实测同一个 GA174 因窗口误读
    /// (C04/CC04/C045)算成 3 个不同候选、连播 3 遍; 窗口只是附属信息, 号码才是
    /// "这一次叫号"的稳定标识。
    /// 4096 条硬上限兜底: 即使 announced_ttl_sec 被配成"永不清理"也不会吃内存。
    announced: AgedMap<String, ()>,
    /// 非阻塞确认状态机: {候选: (累计轮数, 最后出现时刻, 首次出现时刻)}
    /// 关键: 不在确认期间 sleep 阻塞主循环, 否则屏幕连续跳号会被"盲区"吞掉。
    confirm: HashMap<(String, String), (u32, f64, f64)>,
    last_ocr_time: f64,
    /// 观察期截止时刻; > now 表示"有候选待确认", 需要持续触发确认 OCR。
    active_until: f64,
    /// 已预热过 TTS 的候选(第一轮识别到就后台合成, 把合成耗时藏进确认期)。
    /// 集合只增不删但有上界: 满了整表清空, 语义是"近期不重复预热", 足够。
    prefetched: HashSet<(String, String)>,
    /// 兜底复查的空转退避: 连续无候选时把兜底间隔翻倍, 上限 30s;
    /// 一旦有候选(或真实变化)立刻回到基础间隔。防"静置画面每 3s 烧一次 OCR"。
    empty_streak: u32,
    fb_eff: f64,
    /// 连续"无变化"的轮数(静帧快速确认的依据)。
    static_streak: u32,
    /// 当前静帧段起点(最后一条 changed 帧的时刻)。
    static_since: f64,
    /// 「稳帧再认」静置时长(秒)。见 maybe_ocr 注释。0 = 关闭。
    settle_sec: f64,
    /// 变化后静置窗口的截止时刻。now < settle_until 表示"画面还在动, 先不认"。
    settle_until: f64,
    /// 本轮变化是否已被某轮 OCR 认过(防止同一段静止画面被反复触发)。
    change_fired: bool,
    /// 本轮"变化"的最早时刻(静置窗口的最长等待兜底用)。
    change_start: f64,
    /// 上一轮 OCR 是否读出了内容(决定兜底间隔放不放宽)。
    last_read_ok: bool,
    /// 候选首次识别时的原文(静帧快速确认路径没有当轮 raw, 落日志用)。
    /// 同样是"只增不删"的表, 一并纳入时间治理 —— 按号牌累积, 连跑几个月会被撑大。
    cand_raw: AgedMap<(String, String), String>,
    /// 诊断窗口内的 OCR 统计(次数/总耗时/最长), 拼进 [监测] 行便于远程排障。
    diag_ocr_n: u32,
    diag_ocr_total: f64,
    diag_ocr_max: f64,
    diag_last: f64,
    diag_max: f64,
    diag_frames: u64,
    last_save: f64,
    n_saved: u32,
    last_maint: f64,
    /// 上次维护时所在的本地日期(YYYY-MM-DD)。跨日时重置闸门基线, 见 maintenance。
    maint_day: String,
    /// 上次"成功入队播报"的时刻(启动时即置为当前, 避免刚起就误报) —— 健康心跳用。
    last_announce: f64,
    /// 上次打"健康告警"的时刻。用于把告警限流成"每阈值周期最多一条", 防止刷屏。
    last_health_warn: f64,
    /// 作息闸静音提示只打一次的标志(2026-10-22): 回到工作时段复位, 别每帧刷屏。
    mute_warned: bool,
    /// ★2026-10-31: 今天是否工作日(周末/法定节假日 -> false)。带 60s 缓存,
    /// 非工作日整条识别链直接跳过 —— 详见 workday_active_now。
    workday_active: bool,
    /// 上次刷新 workday_active 的时刻(0 = 尚未判定过)。
    workday_checked_at: f64,
    /// 工作日状态切换时只打一行日志, 免得 3fps 下每帧刷屏。
    workday_logged: bool,
    /* —— 号码闸门(2026-10-21) ————————————————————————————————
     * 屏幕改三行列表后, 单数字误读会产出"看起来合理"的号(GA158→GA198、GA150→GA190),
     * 播报后还占住当日去重表, 真号反而不再响。现场编号严格 +1 递增, 用"跳幅上限"
     * 即可拦掉这类误读: 只放行 (上次号, 上次号+max_jump] 区间内的候选。
     */
    /// 每个前缀(GA/G…)上次已播报的号码数值。
    last_num: HashMap<String, u32>,
    /// ★2026-10-23 中危修复(第八轮): 跨日重置已发生、正等待"首个候选"的标记。
    /// 24h 连跑时零点维护节拍清空 last_num, 但屏上还停着昨天的尾号(如 GA352) ——
    /// 旧版任由它走"无基线取最大"直接播报: 零点误播 + 基线被毒化成 352,
    /// 次日 GA001 起全天被闸门(v>last 才放行)静默吞掉。标记生效期间首个无基线
    /// 候选只进当日去重表(不播/不写OK日志/不动基线), 详见 seed_only_after_reset。
    day_reseed: bool,
    /// 跳幅上限。0 = 关闭闸门。
    max_jump: u32,
    /// 号码池上限。超过本值的 OCR 候选不进入确认或静默兜底。
    number_pool_max: u32,
    /// 闸门拦截日志节流时刻(被拦的号每 10s 最多打一行, 防刷屏)。
    gate_log_at: f64,
    /// ★序列重置(2026-10-26): 号码 -> 本静帧段内**首次看见它**的时刻。
    /// 见 SEQ_RESET_SECS / gate_filter。与静帧段绑定: 屏一动(static_since 变)整表
    /// 清空, 于是"同一静帧段内连续稳定 8s"是硬约束, 残影攒不满。触发后整体清空。
    seq_since: HashMap<(String, String), f64>,
    /// 本表所归属的静帧段起点, 用来发现"静帧段已经换了"。
    seq_seg: f64,
    /* —— 多行列表(2026-10-21) ————————————————————————————————
     * 叫号屏改成三行列表后, 最新号不一定在第一行(现场见过落到第二行)。
     * 检测框要覆盖多行才看得见变化, 但识别仍按单行裁 —— 整块喂 tesseract 会掉精度。
     */
    /// 识别行数(1 = 只读 ROI 那一行, 退回旧行为)。
    rows: usize,
    /// 行距(像素), 0 表示不分行。
    pitch: usize,
    /// 帧差检测框 = ROI 首行 +（rows-1)个行距, 覆盖整块列表。
    det_roi: (usize, usize, usize, usize),
    /// 上一轮判出的高亮行(0-based)。只用于"判定结果变化时打一行日志",
    /// 免得每轮 OCR 都刷屏; 现场调光线时这行日志能直接看出判据是否还在工作。
    highlight_row: Option<usize>,
}

/// 号码分解: "GA158" -> Some(("GA", 158))。无数字段返回 None(该候选不参与闸门)。
fn split_num(number: &str) -> Option<(String, u32)> {
    let i = number.find(|c: char| c.is_ascii_digit())?;
    let (pre, digits) = number.split_at(i);
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok().map(|v| (pre.to_string(), v))
}

/// 号码是否落在配置的业务号池范围内。
///
/// 解析不出数字段的候选保持调用方原有语义，不在这里额外拦截。
fn number_pool_accepts(number: &str, max: u32) -> bool {
    match split_num(number) {
        Some((_, value)) => value <= max,
        None => true,
    }
}

/// 剔除超过号池上限的误识别，返回 `(保留候选, 被拒号码)`。
fn filter_number_pool(
    cands: HashSet<(String, String)>,
    max: u32,
) -> (HashSet<(String, String)>, Vec<String>) {
    let mut kept = HashSet::new();
    let mut rejected = Vec::new();
    for (number, window) in cands {
        if number_pool_accepts(&number, max) {
            kept.insert((number, window));
        } else {
            rejected.push(number);
        }
    }
    rejected.sort();
    rejected.dedup();
    (kept, rejected)
}

/// 同批播报的排序键: (前缀, 数值, 窗口)。
/// ★2026-10-23 低危修复(第八轮): 旧版按字符串排, "GA10" < "GA9"('1'<'9'),
///   同批确认多个号时 GA10 抢在 GA9 前播 —— 与现场"小号先叫"的业务顺序相反。
///   拆不出数字段的候选(理论上不会出现, pattern 已保证 \d)退回按原串排兜底。
fn announce_sort_key(n: &str, w: &str) -> (String, u32, String) {
    match split_num(n) {
        Some((pre, v)) => (pre, v, w.to_string()),
        None => (n.to_string(), 0, w.to_string()),
    }
}

/// 启动时从播报日志(CSV)恢复"今天最后一次成功播报的号码"。
///
/// 为什么必须恢复: 重启(改配置就会重启)会清空内存里的 last_num 与去重表,
/// 若闸门没有基线, 第一轮就会把屏上那个"已经播过的号"再播一遍 —— 这正是
/// 2026-10-21 13:56 现场听到的重复播报。只认当天记录, 避免跨天编号重置后
/// 拿昨天的号码当基线, 把早上的低号码全挡在门外。
fn seed_last_numbers(log_file: &str, number_pool_max: u32) -> (HashMap<String, u32>, Vec<String>) {
    use chrono::Local;
    let mut base = HashMap::new();
    let mut announced: Vec<String> = Vec::new();
    let today = Local::now().format("%Y-%m-%d").to_string();
    let text = match std::fs::read_to_string(log_file) {
        Ok(t) => t,
        Err(_) => return (HashMap::new(), announced), // 首次部署还没有日志, 属正常
    };
    for line in text.lines().rev().take(4000) {
        // ★2026-10-23 低危修复: 400 行容易被"最后一条 OK 之后出现 400+ 行 INVALID"
        //   的病态帧(连续 by_change 空读)挤掉当日基线, 重启后屏上已播号会再播一遍
        //   —— 这正是本函数要防的原始事故。日志每行 <100B, 4000 行扫描开销可忽略。
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 5 || cols[cols.len() - 1].trim() != "OK" {
            continue;
        }
        if !cols[0]
            .trim()
            .trim_start_matches('\u{feff}')
            .starts_with(&today)
        {
            continue;
        }
        let num = cols[1].trim().to_string();
        if !number_pool_accepts(&num, number_pool_max) {
            continue;
        }
        if let Some((pre, v)) = split_num(&num) {
            // 倒序扫描: 只认每个前缀"最新"的那条
            base.entry(pre).or_insert(v);
        }
        // 当日全部已播号灌回去重表: 配合漏号补播窗口(BACKFILL_MAX),
        // 重启后屏上残留的"今天已播过"的老号不会被补播窗口误放行;
        // 而重启前就被漏掉、日志里没有的号(如 GA126)不在表里, 重启后
        // 仍能被补播 —— 语义正确。
        announced.push(num);
    }
    (base, announced)
}

/// 系统时间是否"可信": 年份在 [2026, 2035] 内。
/// RTC 纽扣电池没电/断电回退时时钟会跳到出厂值(常见 201x-01-01 或 2000-01-01),
/// 作息闸按坏时间判窗必误判。年份下限取部署年份 2026, 上限留到 2035
/// (2038 问题之前, chrono/时间戳均安全)。纯函数, 供单测。
fn time_plausible_y(year: i32) -> bool {
    (2026..=2035).contains(&year)
}

/// 维护节拍用的"本地日"标识: Unix 秒 -> 本地日期(YYYY-MM-DD)。
/// 取法与 dedupe.rs 的 DailyMidnight 一致(chrono::Local); 时区取不到返回 None,
/// 调用方跳过本次跨日判断 —— 宁可晚重置, 不可误重置。
fn local_day_of(now: f64) -> Option<String> {
    use chrono::{Local, TimeZone};
    Local
        .timestamp_opt(now as i64, 0)
        .single()
        .map(|dt| dt.date_naive().format("%Y-%m-%d").to_string())
}

/// 跨日检测(纯逻辑, 供单测): 发现日期变了就更新 `last_day` 并返回 true。
/// 取不到日期(None)不动状态; 首次观测(last_day 为空, 启动时取日失败)只记录
/// 不当跨日 —— 免得把刚 seed 出来的基线误清。
fn gate_day_rollover(last_day: &mut String, today: Option<String>) -> bool {
    match today {
        Some(d) if last_day.is_empty() => {
            *last_day = d;
            false
        }
        Some(d) if d != *last_day => {
            *last_day = d;
            true
        }
        _ => false,
    }
}

/// 跨日重置标记下的候选判定(纯函数, 供单测): 该候选是否"只标记不播报"。
/// 仅"标记生效 且 前缀尚无基线"时成立 —— 已有基线的候选(理论上刚清完不会有,
/// 但首日真号先于残留号到达时可能已 note_announced)走正常播报, 不被误吞。
fn seed_only_decide(day_reseed: bool, has_baseline: bool) -> bool {
    day_reseed && !has_baseline
}

/// ★2026-10-29 高危修复: 启动时是否**启用**"跨日只标记"(首个候选只标记不播)。
///
/// 原判据只有"基线为空", 把两个完全相反的场景混成了一个:
///   A. 午夜前后启动/跨日 —— 屏上可能挂着昨日尾号(2026-10-26 现场: GA219 被播,
///      基线毒化成 219, 当天 GA002 起的小号全被"号小了"挡死)。
///   B. 每天 08:00 systemd 全新启动 —— 当日 CSV 必然为空(基线必为空), 但此刻屏上
///      是叫号机刚开机的自检页/今日新号, **不存在昨日残留**。原逻辑照 A 处理,
///      把今日第一个真号(GA001)当残留吞掉, 还写进当日去重表 -> 该号当天永久哑
///      (2026-10-29 现场, 存证图 trig_10.jpg 铁证: 屏上 GA001 一个音都没出)。
///
/// 区分两者靠**启动时刻**: 营业时段内启动 = B(放行), 营业时段外启动 = A(只标记)。
/// 号码大小不能用来区分 —— 每日号池 001~220(现场实测日最大叫号 220 以内),
/// 昨日尾号与今日首号落在同一区间, 数值上没有任何分界。
///
/// `now_hm` 为 None 表示系统时间不可信(年份越界), 无法判时段 -> 保守启用。
/// `schedule_enabled=false` 时同样保守启用(没有作息配置就无从判断营业时段)。
fn seed_only_on_startup(
    baseline_empty: bool,
    now_hm: Option<&str>,
    work_start: &str,
    work_end: &str,
    schedule_enabled: bool,
) -> bool {
    if !baseline_empty {
        return false;
    }
    match (schedule_enabled, now_hm) {
        (true, Some(hm)) => !in_work_window(hm, work_start, work_end),
        _ => true,
    }
}

/// ★2026-10-31: 指定日期是否"该播"(纯函数, 供单测)。
///
/// 优先级 **法定节假日 > 补班日 > 星期表**:
///   - `holidays` 命中 -> 休(国庆/春节这种"周一到周五但放假"的日子, 只看星期会空播);
///   - `extra_work_dates` 命中 -> 上(国内调休常见"周六补班", 只看星期会整天漏播);
///   - 其余按 `workdays`(ISO 周几, 1=周一 … 7=周日)判定。
///
/// `enabled=false` 时闸不生效, 恒 true(与作息时间闸的语义保持一致)。
fn schedule_day_active(
    date: chrono::NaiveDate,
    enabled: bool,
    workdays: &[u32],
    extra_work_dates: &[String],
    holidays: &[String],
) -> bool {
    use chrono::Datelike;
    if !enabled {
        return true;
    }
    let key = date.format("%Y-%m-%d").to_string();
    if holidays.iter().any(|d| d.trim() == key) {
        return false;
    }
    if extra_work_dates.iter().any(|d| d.trim() == key) {
        return true;
    }
    workdays.contains(&date.weekday().number_from_monday())
}

/// 今天是不是工作日。跨零点时段(如 22:00~06:00 夜班)的**凌晨段归属起班那天**:
/// 周五 22:00 上的夜班, 到周六 02:00 仍算"周五的班", 否则周五夜班天天被腰斩。
fn workday_ok(now_hm: &str, s: &cfg::Schedule) -> bool {
    let today = chrono::Local::now().date_naive();
    let crosses_midnight = s.work_end.as_str() < s.work_start.as_str();
    let date = if crosses_midnight && now_hm < s.work_end.as_str() {
        today - chrono::Duration::days(1)
    } else {
        today
    };
    schedule_day_active(
        date,
        s.enabled,
        &s.workdays,
        &s.extra_work_dates,
        &s.holidays,
    )
}

/// ★2026-10-26(改动二): 本轮结束后是否要**保住** day_reseed 标记(不清)。
///
/// 语义: 标记的用途是"首个候选只标记不播" —— 它必须在**真的处理过一个候选**
/// (标记或播报)之后才算用完。开机第一轮 OCR 常常读空(屏刚上电闪黑/未聚焦/
/// 换号动画中), 若那一轮就无条件清标记, 下一轮屏上残留的大号(G A219 之流)
/// 就会走"无基线取最大"被播出去并毒化基线, 整个补丁等于白打。
///
/// 所以判据是"本轮有没有候选被处理过"(handled), 而不是"本轮跑过没有"。
fn keep_day_reseed(handled_any_candidate: bool) -> bool {
    !handled_any_candidate
}

/// 多行扫描是否可以在下一行前退出。
///
/// 必须同时满足：至少看过一个候选、候选全部已播、**且已经越过高亮行**。
/// 两点都不能松：
///   - 空集合的 `all()` 恒真, 不能直接用于这里; 否则第 1 行读空会把第 2 行高亮号一起跳过。
///   - 高亮行**未知时必须继续扫**(`is_some_and` 而非 `is_none_or`)。自动判定返回 None
///     的场景(夜间低照度、反光、切回自检页)恰恰是"第 1 行残留旧号、第 2 行才是新号"
///     最容易发生的场景, 此时按旧逻辑 break 就等于把漏播留在兜底路径上。
///     代价是每轮多 1~2 次 OCR; 相比漏播一次的业务代价是划算的(2026-10-27 评审)。
fn should_stop_before_row(
    row_index: usize,
    highlight_row: Option<usize>,
    seen_count: usize,
    all_announced: bool,
) -> bool {
    row_index > 0 && seen_count > 0 && all_announced && highlight_row.is_some_and(|h| row_index > h)
}

/// 固定高亮行优先；配置无效或未配置时退回自动判定。
fn resolve_highlight_row(
    configured: Option<u32>,
    detected: Option<usize>,
    rows: usize,
) -> Option<usize> {
    configured
        .and_then(|v| (1..=rows as u32).contains(&v).then_some(v as usize - 1))
        .or(detected)
}

/// OCR 候选质量：完整窗口 > 仅号码 > 空结果。用于在 psm 主/副档之间选更完整结果，
/// 避免主档只读出号码就提前结束、始终没机会让副档补出窗口。
fn ocr_candidate_quality(cands: &HashSet<(String, String)>) -> u8 {
    if cands.is_empty() {
        0
    } else if cands.iter().any(|(_, w)| !w.trim().is_empty()) {
        2
    } else {
        1
    }
}

fn enhancement_pixels_allowed(width: usize, height: usize) -> bool {
    width <= 1280 && (width as u64).saturating_mul(height as u64) <= MAX_ENHANCE_PIXELS
}

type RankedOcrResult = (String, HashSet<(String, String)>, u8);

/// 作息闸判定(纯函数, 便于单测): 当前 "HH:MM" 是否落在 start~end 工作窗口内。
/// start<=end 为普通窗口(含 start、不含 end); start>end 为跨零点窗口
/// (如 22:00~06:00 夜班); start==end 视为全天。任一侧解析失败 => false
/// —— 宁可静音由告警暴露, 不可带着坏配置整夜乱播。
fn in_work_window(now: &str, start: &str, end: &str) -> bool {
    use chrono::NaiveTime;
    let (Some(n), Some(s), Some(e)) = (
        NaiveTime::parse_from_str(now, "%H:%M").ok(),
        NaiveTime::parse_from_str(start, "%H:%M").ok(),
        NaiveTime::parse_from_str(end, "%H:%M").ok(),
    ) else {
        return false;
    };
    if s == e {
        return true;
    }
    if s < e {
        n >= s && n < e
    } else {
        n >= s || n < e
    }
}

impl Loop {
    /// 启动期一次性构造: 解析 ROI、建 OCR / 播报器 / 取流通道, 初始化全部状态。
    fn build(cfg: Config) -> Result<Loop, String> {
        let roi_v = cfg
            .roi
            .clone()
            .filter(|r| r.len() == 4)
            .ok_or("请先运行 calibrate.py 框选识别区域")?;
        let roi = (
            roi_v[0] as usize,
            roi_v[1] as usize,
            roi_v[2] as usize,
            roi_v[3] as usize,
        );

        let ocr = ocr::Ocr::new(&cfg.ocr)?;
        let ann = announcer::Announcer::new(&cfg)?;
        let src = capture::open(&cfg.source(), &cfg)?;

        let det = cfg.detect.clone();
        let fb_sec = det.fallback_ocr_sec;
        // 多行列表: 行距既可用绝对像素(row_pitch), 也可用相对行高(row_pitch_ratio)。
        // 行距为 0 时强制退化为单行, 免得"rows=3 但步长 0"把同一行读三遍。
        let pitch = resolve_row_pitch(det.row_pitch, det.row_pitch_ratio, roi.3);
        if det.row_pitch > 0 && roi.3 > 0 {
            // ★标定护栏: 换码流后最容易踩的坑就是沿用旧分辨率下行距的"按比例换算值"。
            //   真实行距几乎等于行高(实测 69 vs 70), 超过 1.6 倍几乎一定是没重新标定。
            let ratio = det.row_pitch as f64 / roi.3 as f64;
            if ratio > 1.6 {
                println!(
                    "[警告] row_pitch={} 是行高 {} 的 {:.1} 倍, 疑似未按当前分辨率重新标定\
                     (真实行距通常≈行高, 建议改用 row_pitch_ratio=1.0)",
                    det.row_pitch, roi.3, ratio
                );
            }
        }
        let rows = if pitch == 0 {
            1
        } else {
            det.rows.max(1) as usize
        };
        // 检测框向下扩到覆盖 rows 行; 识别仍按 pitch 逐行裁。
        let det_roi = (roi.0, roi.1, roi.2, roi.3 + (rows - 1) * pitch);
        let now = now_secs();
        // 开机 seed: 基线(每前缀最新 OK 号) + 当日全部已播号(灌去重表)。
        let (seed_baselines, seed_announced) =
            seed_last_numbers(&cfg.log_file, det.number_pool_max);
        // ★2026-10-26: 当天一条 OK 记录都没有 => 基线为空 => 屏上残留的大号会被
        //   "无基线取最大"当新号播掉并毒化基线。见下面 day_reseed 的赋值。
        let seed_baselines_is_empty = seed_baselines.is_empty();
        // ★2026-10-29: 跨日"只标记"只在**非营业时段启动**才启用。每日 08:00
        //   systemd 全新启动时当日 CSV 必为空, 但屏上是今日新号而非昨日残留;
        //   沿用旧判据会把今日首号(GA001)当残留吞掉并写进去重表 -> 当天永久哑。
        //   判据与取舍详见 seed_only_on_startup。
        let startup_seed_only = {
            let now_lc = chrono::Local::now();
            let hm = if time_plausible_y(now_lc.year()) {
                Some(now_lc.format("%H:%M").to_string())
            } else {
                None
            };
            let enable = seed_only_on_startup(
                seed_baselines_is_empty,
                hm.as_deref(),
                &cfg.schedule.work_start,
                &cfg.schedule.work_end,
                cfg.schedule.enabled,
            );
            if enable {
                println!(
                    "[闸门] 启动时基线为空且处于非营业时段({}): 首批候选只标记不播, \
                     防屏上残留大号(如昨日尾号)毒化基线 —— 详见 seed_only_after_reset",
                    hm.as_deref().unwrap_or("时间不可信")
                );
            } else if seed_baselines_is_empty {
                println!(
                    "[闸门] 启动时基线为空但处于营业时段({}): 不启用跨日只标记, \
                     今日首号正常播报(防每日首个真号被当残留吞掉)",
                    hm.as_deref().unwrap_or("-")
                );
            }
            enable
        };
        Ok(Loop {
            interval: 1.0 / cfg.fps.max(0.1),
            need_stable: det.stable_frames,
            ttl: det.confirm_ttl_sec,
            active_sec: det.active_sec,
            fb_sec,
            static_fb_sec: det.static_fb_sec,
            confirm_ticks: det.static_confirm_ticks,
            diag_on: cfg.diag_heartbeat,
            diag_every: cfg.diag_every_sec,
            save_frames: cfg.save_trigger_frames,
            maint_every: 60.0,
            log_max_bytes: (cfg.log_max_mb.max(0.0) * 1024.0 * 1024.0) as u64,
            health_alert_sec: cfg.health_alert_min.max(0.0) * 60.0,

            ocr,
            ann,
            src,

            prev_gray: None,
            roi_warned: false,
            // 已播报去重表: 带时间维度, 见 dedupe.rs 与字段注释。
            // 开机先把当日日志里的 OK 号灌进来(漏号补播窗口的前提:
            // 屏上"今天已播过"的残留必须靠这张表压住)。
            announced: {
                let mut m = AgedMap::new(Expiry::from_ttl_sec(cfg.announced_ttl_sec), 4096);
                for n in seed_announced {
                    m.insert(n, (), now);
                }
                m
            },
            confirm: HashMap::new(),
            last_ocr_time: 0.0,
            active_until: 0.0,
            prefetched: HashSet::new(),
            empty_streak: 0,
            fb_eff: fb_sec,
            static_streak: 0,
            static_since: 0.0,
            settle_sec: det.settle_sec.max(0.0),
            settle_until: 0.0,
            change_fired: true,
            change_start: 0.0,
            last_read_ok: false,
            cand_raw: AgedMap::new(Expiry::from_ttl_sec(cfg.announced_ttl_sec), 8192),
            diag_ocr_n: 0,
            diag_ocr_total: 0.0,
            diag_ocr_max: 0.0,
            diag_last: now,
            diag_max: 0.0,
            diag_frames: 0,
            last_save: 0.0,
            n_saved: 0,
            last_maint: now,
            last_announce: now,
            last_health_warn: now,

            last_num: seed_baselines,
            maint_day: local_day_of(now).unwrap_or_default(),
            mute_warned: false,
            // 保守初值 true: 首轮 workday_active_now() 会立刻重算并打一行日志。
            workday_active: true,
            workday_checked_at: 0.0,
            workday_logged: false,
            // ★2026-10-26 高危修复: 基线为空时闸门走"无基线取最大", 屏上残留的大号
            //   (如昨日尾号 GA219)会被当新号播掉并毒化基线; 之后当天 GA002 起的小号
            //   全被"号小了差>5"挡死(2026-10-26 上午现场真实事故)。
            //   代码里原本只有"零点跨日"路径置这个标记(maintenance), 重启这条路径没接。
            // ★2026-10-29 收窄: 只在**非营业时段启动**才启用。08:00 全新启动时屏上是
            //   今日新号而非昨日残留, 照旧启用会把 GA001 当残留吞掉且当天永久哑。
            day_reseed: startup_seed_only,
            max_jump: det.max_jump,
            number_pool_max: det.number_pool_max,
            gate_log_at: 0.0,
            seq_since: HashMap::new(),
            seq_seg: 0.0,

            rows,
            pitch,
            det_roi,
            highlight_row: None,

            det,
            roi,
            cfg,
        })
    }

    /// 主循环骨架。只做三件事: 停止检查、跑一轮、按 fps 补睡眠。
    fn run(&mut self) {
        println!(
            "运行中: 源={}, ROI=({}, {}, {}, {}), 稳定帧数={}, Ctrl+C 退出",
            src_desc(&self.cfg),
            self.roi.0,
            self.roi.1,
            self.roi.2,
            self.roi.3,
            self.det.stable_frames
        );

        // ★2026-10-31: 启动即判一次工作日。非工作日这一行必须打出来, 否则现场
        //   看到"服务 active 但一声不响、日志也不长"会以为坏了。
        if self.cfg.schedule.enabled {
            self.workday_active_now();
        }

        loop {
            // ★优雅退出检查点: 收到 SIGTERM/SIGINT 就在这里 break, 而不是被信号打死。
            //   正常返回 -> 本结构体析构 -> PipeGrabber 收 ffmpeg。
            if signal::stopped() {
                println!("[退出] 收到停止信号(SIGTERM/SIGINT), 开始收尾…");
                break;
            }
            let loop_start = now_secs();
            if matches!(self.tick(), Tick::SkipSleep) {
                continue;
            }
            let rest = self.interval - (now_secs() - loop_start);
            std::thread::sleep(Duration::from_secs_f64(rest.max(0.05)));
        }

        // —— 优雅退出收尾 ————————————————————————————————————————
        // 先把"已确认待播"的叫号播完(有界 10s), 再让 PipeGrabber 析构收 ffmpeg。
        // 顺序有讲究: 先播完再拆流, 否则重启瞬间正在排队的号会被掐掉。
        if !self.ann.drain(Duration::from_secs(10)) {
            println!("[退出] 播报队列仍有积压, 超时退出(未播完的号码已记入 CSV 日志)");
        }
        println!("[退出] 已优雅停机");
    }

    /// 一轮主循环: 取帧 -> 帧差/静帧 -> 诊断 -> 快速确认 -> (按需)OCR -> 维护。
    fn tick(&mut self) -> Tick {
        let frame = match self.src.read() {
            Some(f) => f,
            None => {
                println!("[错误] 取帧失败，1秒后重试");
                std::thread::sleep(Duration::from_secs(1));
                self.prev_gray = None;
                // ★2026-10-22 第五轮审查: 早退也必须跑维护与健康心跳 —— 断流期间
                //   恰是"链路死了"的时段, 之前 dedupe 清理/日志轮转/跨日重置/
                //   [健康] 告警全被跳过, 与健康心跳的设计初衷正好相反。
                //   maintenance 内部自带节流, 每秒调一次是安全的。
                let now = now_secs();
                self.maintenance(now);
                self.health_check(now);
                return Tick::SkipSleep;
            }
        };

        // —— 预览帧按需落盘 ———————————————————————————————
        // 配置台在 _live/req 放一个文件即请求一次; 落盘后立刻删除请求。
        // 这样 Web 预览不需要再开一路 ffmpeg(USB 设备被本进程独占, 直接抓会 busy),
        // 空闲时也完全不写盘(省 SD 卡寿命)。
        // ★与抓帧线程共用 capture::serve_preview(原子写 tmp+rename、日志节流):
        //   这里原先自己写了一份裸 File::create("latest.jpg") 的实现, 没有原子替换,
        //   谁先看到 req 谁写 —— 主循环抢到时配置台就会读到半张图。
        capture::serve_preview(&frame, "主循环");

        // 帧差用"检测框"(rows>1 时覆盖整块列表), 这样第 2/3 行换号也能被看见。
        let prepared = detect::prepare(&frame, self.det_roi);
        // ROI 落在帧外(源尺寸变化/配置不匹配): 跳帧防 panic, 只告警一次不刷屏
        if prepared.gray.width() == 0 {
            if !self.roi_warned {
                self.roi_warned = true;
                eprintln!(
                    "[警告] ROI {:?} 超出帧 {}x{}, 跳帧等待恢复",
                    self.det_roi, frame.w, frame.h
                );
            }
            // ★2026-10-22 第五轮审查: 与取帧失败同理 —— ROI 越界(换码流分辨率)期间
            //   健康心跳/维护不能停, 否则告警一次后就零日志, 正是"进程活着但链路死了"。
            let now = now_secs();
            self.maintenance(now);
            self.health_check(now);
            return Tick::SkipSleep;
        }
        let mut prepared = prepared;
        detect::blur5(&mut prepared.gray);

        let mut changed = true;
        let mut ratio: Option<f64> = None;
        if let Some(prev) = &self.prev_gray {
            // 先抵消"整体亮度漂移"再比: 摄像头自动曝光会持续微调整帧亮度,
            // 原始 absdiff 会把漂移当"画面变化"导致空场景反复触发 OCR。
            if let Some(r) = detect::change_ratio(&prepared.gray, prev, self.det.diff_threshold) {
                ratio = Some(r);
                changed = r > self.det.changed_ratio;
            }
        }
        self.prev_gray = Some(prepared.gray.clone());

        let now = now_secs();

        // —— 静帧计数 ——————————————————————————————————————————
        // changed=true 的那一轮是"静帧段"起点; 之后每连续一轮无变化 streak+1。
        // 快速确认、静态兜底间隔都建立在它上面。
        if changed {
            self.static_streak = 0;
            self.static_since = now;
            // ★稳帧再认: 画面刚变 -> 起一段静置窗口, 期间不 OCR(见 maybe_ocr)。
            // 换号动画(滚动/淡入)期间每一帧都"有变化", 旧逻辑会立刻拿动画中间帧去认,
            // 读出的错号(GA174->GAT174)正是现场误播报的来源。
            if self.change_fired {
                self.change_start = now; // 新一轮"变化"的起点(最长等待兜底用)
            }
            self.settle_until = now + self.settle_sec;
            self.change_fired = false;
        } else {
            self.static_streak = self.static_streak.saturating_add(1);
        }

        self.diag_heartbeat(now, ratio);
        self.fast_confirm();
        self.maybe_ocr(&frame, changed, now);
        self.maintenance(now);
        self.health_check(now);

        Tick::Done
    }

    /// 诊断心跳: 每 diag_every 秒报一次"近N秒最大帧差"。
    /// 排查"举了卡却没反应"时靠它区分【没举卡】和【阈值漏检】。
    fn diag_heartbeat(&mut self, now: f64, ratio: Option<f64>) {
        if let Some(r) = ratio {
            if r > self.diag_max {
                self.diag_max = r;
            }
        }
        self.diag_frames += 1;
        if !(self.diag_on && now - self.diag_last >= self.diag_every) {
            return;
        }
        let span = now - self.diag_last;
        let tag = if self.diag_max > self.det.changed_ratio {
            ">>> 有变化"
        } else {
            "静"
        };
        let ocr_info = if self.diag_ocr_n > 0 {
            format!(
                " OCR={}次 均匀{:.1}s 最长{:.1}s",
                self.diag_ocr_n,
                self.diag_ocr_total / f64::from(self.diag_ocr_n),
                self.diag_ocr_max
            )
        } else {
            " OCR=0次".to_string()
        };
        // 帧龄只在偏大时才打: 正常几秒内一直刷没意义, 但超过 5s 就是"流僵死"的先兆
        // (画面静止时帧差恒为 0, 光看帧差分不出"没人叫号"和"流死了", 帧龄能)。
        let age_info = match self.src.frame_age_sec() {
            Some(a) if a > 5.0 => format!(" 帧龄={:.0}s", a),
            _ => String::new(),
        };
        println!(
            "[监测] 近{:.0}s 最大帧差={:.4} 阈值={:.4} 帧数={} {}{}{}",
            span, self.diag_max, self.det.changed_ratio, self.diag_frames, tag, ocr_info, age_info
        );
        self.diag_last = now;
        self.diag_max = 0.0;
        self.diag_frames = 0;
        self.diag_ocr_n = 0;
        self.diag_ocr_total = 0.0;
        self.diag_ocr_max = 0.0;
    }

    /// ★2026-10-31: 今天是不是工作日(带 60s 缓存)。
    ///
    /// 非工作日(周末/法定节假日)整条识别链在 `maybe_ocr` 里最先被跳过 —— 现场
    /// 2026-10-31(周六) 半天就往 CSV 写了 14 条 INVALID(全是玻璃反光和"设计部门
    /// 回球"公告牌的文字), 既白烧 CPU 又污染播报日志。跳过只影响 OCR/播报,
    /// 取帧、配置台预览、健康心跳照常 —— 周末想远程看一眼摄像头仍然可以。
    ///
    /// 缓存 60s: 主循环约 3fps, 每帧都格式化日期/比对日期表没必要; 一分钟的
    /// 切换延迟对"跨天"这种量级的事件完全够用。
    fn workday_active_now(&mut self) -> bool {
        let now = now_secs();
        if self.workday_checked_at > 0.0 && now - self.workday_checked_at < 60.0 {
            return self.workday_active;
        }
        self.workday_checked_at = now;
        let hm = chrono::Local::now().format("%H:%M").to_string();
        let active = workday_ok(&hm, &self.cfg.schedule);
        if !self.workday_logged || active != self.workday_active {
            self.workday_logged = true;
            if active {
                println!("[作息] 今天是工作日, 正常识别播报");
            } else {
                println!(
                    "[作息] 今天({})非工作日(周末/法定节假日), 跳过识别与播报; 屏面预览仍可用",
                    chrono::Local::now().format("%Y-%m-%d")
                );
            }
        }
        self.workday_active = active;
        active
    }

    /// 作息闸统一入口(2026-10-23 高危修复): schedule.enabled 且当前不在工作时段
    /// -> false。两条播报路径(fast_confirm 静帧快路 / announce_winners 投票路)
    /// 都必须过这一关 —— 旧版只闸投票路, 现场主力是静帧快路, "夜里整夜播报"
    /// 的修复被快路打穿。静音提示只打一次, 回到窗口复位; 静音不写 announced/
    /// 日志/基线, 恢复后照常播。health_check 的"链路失效"告警也要做同样豁免。
    fn announcement_allowed(&mut self) -> bool {
        if !self.cfg.schedule.enabled {
            return true;
        }
        // ★时间可信度守卫(2026-10-23): RTC 纽扣电池没电/断电回退会让时钟跳到
        //   出厂值, 按坏时间判窗会把工作时间错判成休息时间 —— 该播的时候整场
        //   静音、 systemd 侧还会把服务停掉。时间明显不对(年份不在可信区间)
        //   时作息闸自动失效, 照常播报 —— 宁可错播, 不可错停。
        let now_lc = chrono::Local::now();
        if !time_plausible_y(now_lc.year()) {
            if !self.mute_warned {
                self.mute_warned = true;
                println!(
                    "[时间] 系统时间明显异常({}, 作息闸已自动失效, 照常播报; 请校时)",
                    now_lc.format("%Y-%m-%d %H:%M:%S")
                );
            }
            return true;
        }
        let hm = now_lc.format("%H:%M").to_string();
        // ★2026-10-31: 周末/法定节假日一律不播(补班日已在 workday_ok 里按"强制上班"处理)。
        //   与时间闸分开判: 时间闸只管"每天几点到几点", 这里管"哪些天要上班"。
        if !self.workday_active_now() {
            self.mute_warned = true;
            return false;
        }
        let inside = in_work_window(
            &hm,
            &self.cfg.schedule.work_start,
            &self.cfg.schedule.work_end,
        );
        if inside {
            if self.mute_warned {
                self.mute_warned = false;
                println!("[作息] 已进入工作时段, 恢复播报");
            }
            return true;
        }
        if !self.mute_warned {
            self.mute_warned = true;
            println!(
                "[作息] 当前 {} 不在工作时段 {}~{}, 静音中(OCR 照常, 到点自动续播)",
                hm, self.cfg.schedule.work_start, self.cfg.schedule.work_end
            );
        }
        false
    }

    /// 静帧快速确认: 候选出现过一次, 且画面已连续 confirm_ticks 轮无变化 -> 直接确认
    /// 播报, 免掉第二轮 OCR。原理: tesseract 对相同输入是确定性的, 像素不变复读必然
    /// 同文; 第二轮唯一防的是"转瞬即逝的假影", 而静帧观察直接测量了"画面稳定",
    /// 防护等价, 却省下一整轮 OCR(靶机实测 1.6~4.8s —— 5~10s 延迟的最大单项)。
    /// 防误报关键: 候选必须是在"当前静帧段内" sighting 的(last_seen >= static_since)。
    /// 反光假影在晃动期被读到, 晃动停止后 static_since 更新, 假影因 last_seen
    /// 落在旧段而永远凑不齐快速确认 -> 只能走原投票路径(和旧行为一致)。
    /// 观察期内一旦又出现变化(changed=true), streak 清零, 自动退回两轮投票。
    fn fast_confirm(&mut self) {
        // ★2026-10-23 高危修复: 播报前过作息闸 —— 本函数是现场主力播报路径
        //   (静帧稳住即播, 免第二轮 OCR), 不闸的话静音期照常出声。
        if !self.announcement_allowed() {
            return;
        }
        if self.confirm_ticks < 1
            || self.static_streak < self.confirm_ticks
            || self.confirm.is_empty()
        {
            return;
        }
        // 与正式播报路径同口径: 按【号码】聚合, 窗口取票数最多的那条。
        // ★分档确认(2026-10-21): 跳幅越大, 多要几个静帧 tick 才放行 —— 普通 +1/+2
        //   仍是原来的 2 tick 立即播; +4~+10 多等约 0.6s; 大过号多等约 1.2s。
        //   静帧 tick 是"画面确实稳住"的直接测量, 比多跑一轮 OCR(1.5~5s)便宜得多。
        let streak = self.static_streak;
        let mut hit: Vec<(String, String, u32, f64, f64)> = number_votes(&self.confirm)
            .into_iter()
            // last_seen >= static_since: 候选必须 sighting 于"当前静帧段"内,
            // 旧静帧段残留的假影候选永远进不了快速确认(走原投票路径)。
            .filter(|(n, _, _, _, last)| {
                !self.announced.contains_key(n)
                    && *last >= self.static_since
                    && streak >= self.confirm_ticks + self.jump_extra(n)
            })
            .collect();
        // ★2026-10-26(改动二): 本路径也要"真的处理过至少一个候选才清 day_reseed",
        //   否则开机首轮 OCR 读空(屏刚上电闪黑/换号动画中)会把标记白白清掉, 下一轮
        //   那个残留大号就趁虚而入毒化基线。这里 hit 为空 = 本轮没有候选可处理,
        //   保住标记等下一轮(判据抽成纯函数, 见 keep_day_reseed)。
        if hit.is_empty() {
            return;
        }
        // 真正进入处理流程的候选数(见下方 day_reseed 清理条件)。
        let mut handled = false;
        // 元组含 f64(无 Ord), 走排序键: (前缀, 数值, 窗口) —— 数值序与业务编号
        // 顺序一致(GA9 先于 GA10), 字符串序会把 GA10 排到 GA9 前(第八轮低危)。
        hit.sort_by_key(|a| announce_sort_key(&a.0, &a.1));
        let t_ann = now_secs();
        for (number, window, _votes, first, _last) in hit {
            handled = true;
            if self.seed_only_after_reset(&number, t_ann) {
                continue;
            }
            let extra = self.jump_extra(&number); // 必须在 note_announced 之前算
            self.announced.insert(number.clone(), (), t_ann);
            self.note_announced(&number);
            let raw = self
                .cand_raw
                .get(&(number.clone(), window.clone()))
                .cloned()
                .unwrap_or_default();
            logger::log(&self.cfg.log_file, &number, &window, &raw, "OK");
            self.ann.speak(&number, &window); // 入FIFO队列, 播报线程按顺序播
            self.last_announce = t_ann; // 健康心跳: 真有号播了, 复位闲置计时
            println!(
                "[时序] 首见→开始播报 {:.0} ms (静帧确认{}轮, 免第二轮OCR, 跳幅多确认+{}); 另加相机/网络/轮询约 0.3~0.8 s",
                (t_ann - first) * 1000.0,
                self.static_streak,
                extra
            );
        }
        // 首批候选处理完即清跨日标记 —— 残留号已进当日去重表, 后续轮次由去重表
        // 接管压制; 次日真号(如 GA001)走"无基线取最大"照常播报, 不再被本标记误吞。
        // ★2026-10-26(改动二): 只在"本轮真的处理过候选"时才清(判据见 keep_day_reseed)。
        if !keep_day_reseed(handled) {
            self.day_reseed = false;
        }
        // 已无待确认候选: 撤观察期。否则 active_until 还会让主循环
        // 再空跑一轮"确认 OCR"(2~5s CPU), 白占机器。
        self.active_until = 0.0;
    }

    /// 按需跑一轮 OCR 并推进确认状态机。
    ///
    /// 只做编排: 五个步骤各自独立成方法, 便于分段阅读与单测 ——
    ///   ocr_due              该不该认、是不是"为本轮变化"而认(纯判定)
    ///   save_evidence        存定型帧(仅"变化路径", 不是换号动画的过渡帧残影)
    ///   recognize_candidates OCR + 增强图兜底链路
    ///   ingest_candidates    诊断 / 确认窗口 / 退避 / 预热 / 确认状态机 / 票决播报
    ///   note_ocr_failure     失败归账(同样进诊断与空转退避)
    fn maybe_ocr(&mut self, frame: &Arc<capture::Frame>, changed: bool, now: f64) {
        // ★2026-10-31: 非工作日(周末/法定节假日)整条识别链在这里就断掉。
        //   周末屏上没人叫号, 照常 OCR 只会把反光、公告牌文字认成一堆 INVALID
        //   写进 CSV(10-31 周六半天 14 条), 白烧 CPU 还污染播报日志。
        //   放在最前面: 连存证图都不落盘, 一举把 _live/ 和 tesseract 都省掉。
        //   注意与作息时间闸的区别 —— 时间闸只是"静音"(OCR 照常, 为了到点续播),
        //   这里是"这一天本来就不上班", 不需要续播, 直接不认。
        if self.cfg.schedule.enabled && !self.workday_active_now() {
            return;
        }
        let by_change = match self.ocr_due(changed, now) {
            Some(v) => v,
            None => return,
        };
        self.last_ocr_time = now;
        // 本轮已覆盖这次变化(不论由哪条路径触发), 同一段静止画面不再重复触发。
        self.change_fired = true;
        let iter_t0 = now_secs();
        // 存证图存"检测区"(rows>1 时是三行整块), 排查时一眼能看到全貌;
        // 识别则按行分别裁 —— 整块喂给 tesseract 会明显掉识别率(现场实测)。
        let det_box = capture::crop_rgb(frame, self.det_roi);
        self.save_evidence(&det_box, by_change, now);
        match self.recognize_candidates(frame) {
            Ok((raw, cands)) => {
                let iter_secs = now_secs() - iter_t0;
                self.ingest_candidates(cands, &raw, iter_secs, by_change, now);
            }
            Err(e) => {
                let iter_secs = now_secs() - iter_t0;
                self.note_ocr_failure(&e, iter_secs);
            }
        }
    }

    /// 本轮该不该跑 OCR(纯判定, 不改状态)。
    ///
    /// 返回 `Some(by_change)`: 要跑, 且 `by_change=true` 表示这次是"为这一轮变化"
    /// 而认(决定要不要存证定型帧); 返回 `None`: 不跑。稳帧细节见 `settle_ready`。
    ///
    /// 触发路径并联(全部要求"画面已定型"):
    ///   ① 帧差判定有变化 -> 静置 settle_sec 后认一次(稳帧再认, 现场调优)
    ///   ② 处于观察期(有候选待确认) -> 完成多轮确认
    ///   ③ 距上次 OCR 超过兜底间隔 -> 保险丝(防 ROI 略大时帧差漏检);
    ///      间隔自适应: 屏面静止且上轮读出过内容时放宽到 static_fb_sec(省 CPU)。
    fn ocr_due(&self, changed: bool, now: f64) -> Option<bool> {
        let gap = now - self.last_ocr_time;
        let (ready, by_change) =
            settle_ready(now, self.settle_until, self.change_fired, self.change_start);
        if !ready {
            return None;
        }
        // ①②受 min_interval 节流; ③(兜底保险丝)不受它管 —— 它自带自适应间隔。
        let throttled = gap < self.det.min_interval_sec;
        if (by_change || now < self.active_until) && !throttled {
            return Some(by_change);
        }
        let ins_gap = if changed || !self.last_read_ok {
            self.fb_eff
        } else {
            self.static_fb_sec.max(self.fb_eff)
        };
        if self.fb_sec > 0.0 && gap >= ins_gap {
            return Some(by_change);
        }
        None
    }

    /// 触发存证: 因"画面变化"跑 OCR 时把所看画面存图,
    /// 排查「心跳有变化但读不出」时直接看图。
    ///
    /// ★条件用 `by_change`(静置后为本轮变化跑的那一次)而不是"这一轮有没有变" ——
    ///   现在存的是**定型帧**, 不再是一动就抓到的过渡帧残影(现场调优 2026-10-13)。
    fn save_evidence(&mut self, crop: &image::RgbImage, by_change: bool, now: f64) {
        if !(self.save_frames && by_change && now - self.last_save >= 2.0 && self.n_saved < 20) {
            return;
        }
        // ★2026-10-23 低危修复: 存证满 20 张后旧版静默停用 —— 排障时以为还在
        //   存证其实早停了。到达上限只提示一次, 别每帧刷屏。
        if self.n_saved + 1 == 20 {
            println!("[存证] 已达 20 张上限, 本进程内停止存证(重启后重计)");
        }
        self.last_save = now;
        self.n_saved += 1;
        let _ = std::fs::create_dir_all(SAVE_DIR);
        let p = format!("{}/trig_{:02}.jpg", SAVE_DIR, self.n_saved);
        let res = std::fs::File::create(&p).and_then(|f| {
            let mut enc =
                image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(f), 92);
            enc.encode_image(&image::DynamicImage::ImageRgb8(crop.clone()))
                .map_err(std::io::Error::other)
        });
        match res {
            Ok(()) => println!("[存证] OCR所见画面 -> {}", p),
            Err(e) => println!("[存证失败] {}", e),
        }
    }

    /// 多行列表识别: 按行距逐行裁切、逐行 OCR, 返回 `(本轮原始文本, 候选集合)`。
    ///
    /// ★为什么按行裁而不是把三行框一起喂给 tesseract(2026-10-21 现场实测):
    ///   同一帧、同样参数下, 三行整块 psm11 读出来是 "D理 10， GA1 TD" 这类碎片;
    ///   切回单行 220x49 立刻恢复 "请GA192到C06号窗口办理"。原因是这块屏每行只有
    ///   约 24px 高, 行间还有色带分隔, tesseract 在整块上的版面切分一乱, 号码就断。
    ///   所以检测框覆盖多行、识别仍按单行做, 两者互不影响。
    ///
    /// 提前退出: ★2026-10-23 现场 GA126 事故后改为"所见皆已播才停" ——
    /// 原条件(第 1 行命中 gate_accepts 的"该播的号"就停)会在新号顶到第 1 行时
    /// 跳过第 2/3 行, 被连续叫号顶下去的漏号从此永远读不到。改为: 目前读到的
    /// 候选全部已播过(纯稳态老号屏)才停 —— 稳态仍只读 1 行不费 CPU;
    /// 有新号时多读两行, 恰好是要抓漏号的时机。
    ///
    /// ★2026-10-23 低危注释(第八轮): 无基线(刚部署/跨日重置)时新号必有候选,
    ///   会继续读完 2/3 行; gate_filter 的"无基线取最大"仍只播最大号。
    ///   可接受: 业务上最新号在首行, 2/3 行的残留号走补播窗口另有闸口。
    ///   若现场版式反过来(最新在底部), 应把 rows 配准让首行命中不了候选,
    ///   自然会读完全部行。
    ///
    /// ★2026-10-27 按行分流: 三行列表里有一行是"浅蓝高亮底"(当前办理), 它的
    ///   对比度只有深底行的一半, 旧增强链会把浅底判成字、把这一行读废(实测
    ///   行2 完整率 39%, 行1/行3 是 99%)。这里先量各行底色, 挑出最亮的那个
    ///   (相对比较, 不吃自动曝光漂移), 只有它走 `enhance_highlight_for_ocr`。
    fn recognize_candidates(
        &mut self,
        frame: &Arc<capture::Frame>,
    ) -> Result<(String, HashSet<(String, String)>), String> {
        let rows = self.rows.max(1);
        let mut cands: HashSet<(String, String)> = HashSet::new();
        let mut raws: Vec<String> = Vec::new();
        // ★2026-10-27: 先把各行裁出来、量一遍底色亮度, 找出"高亮行"。
        //   裁定必须在识别之前 —— 每行走哪条增强链依赖这个结果, 而认完第 1 行
        //   可能就提前退出了, 到时候再补算就会漏掉没裁过的行。
        //   裁剪成本可忽略(3 行 × 570×69×3 ≈ 350KB), OCR 一次是 1.5s 量级。
        let mut crops: Vec<image::RgbImage> = Vec::with_capacity(rows);
        for i in 0..rows {
            let y = self.roi.1 + i * self.pitch;
            // ★2026-10-22 第五轮审查: pitch==0(单行/默认配置)时首轮 i=0 也会命中,
            //   连第一行都不裁不认 -> 候选恒空 -> 整条播报链静默全灭(93912f3 多行
            //   重构引入; 现场三行配置恰好配了 row_pitch 才没暴露)。
            //   "pitch==0 不再切下一行"只该约束 i>0; 单行模式下第一行必须照常识别。
            if (i > 0 && self.pitch == 0) || y + self.roi.3 > frame.h {
                break;
            }
            crops.push(capture::crop_rgb(
                frame,
                (self.roi.0, y, self.roi.2, self.roi.3),
            ));
        }
        let lumas: Vec<u8> = crops.iter().map(detect::median_luma_red).collect();
        // 实测最亮行与最终裁定分开存: 现场把 highlight_row 固定成 2 之后, 版式一变
        // 就只会"静默地用错行", 日志里必须能同时看到配置值和实测值才排得出来。
        let detected = detect::pick_highlight_row(&lumas);
        let highlight_row = resolve_highlight_row(self.det.highlight_row, detected, rows);
        // 判定结果变化时才打日志: 现场光照/版式一变, 这行能立刻看出判据还准不准。
        if self.highlight_row != highlight_row {
            match highlight_row {
                Some(h) => {
                    println!(
                        "[识别] 高亮行=第{}行(各行底色亮度{:?}, 配置={:?}, 实测最亮={:?}), 该行改走高亮专用增强",
                        h + 1,
                        lumas,
                        self.det.highlight_row,
                        detected.map(|d| d + 1)
                    );
                    if let Some(d) = detected.filter(|d| *d != h) {
                        println!(
                            "[警告] 高亮行配置与实测不一致: 配置=第{}行, 实测最亮=第{}行 —— 若已换版式/分辨率, 请同步 highlight_row 或改回 0(自动)",
                            h + 1,
                            d + 1
                        );
                    }
                }
                None => println!(
                    "[识别] 未识别到高亮行(各行底色亮度{:?}, 配置={:?}), 全部走常规链路, 逐行扫满不提前退出",
                    lumas, self.det.highlight_row
                ),
            }
            self.highlight_row = highlight_row;
        }
        for (i, crop) in crops.iter().enumerate() {
            let all_announced =
                !cands.is_empty() && cands.iter().all(|c| self.announced.contains_key(&c.0));
            if should_stop_before_row(i, highlight_row, cands.len(), all_announced) {
                break;
            }
            // 第一行失败才真报错(与单行时代语义一致); 后续行失败只当这一行没读出来,
            // 不能因为第二行糊了就把第一行已经读到的结果丢掉。
            let (raw_i, c_i) = match self.recognize_one(crop, highlight_row == Some(i)) {
                Ok(v) => v,
                Err(e) if i == 0 => return Err(e),
                Err(_) => continue,
            };
            if !raw_i.trim().is_empty() {
                raws.push(if rows > 1 {
                    format!("[行{}]{}", i + 1, raw_i)
                } else {
                    raw_i.clone()
                });
            }
            cands.extend(c_i);
        }
        // 同一号码若另有带窗口的候选, 丢弃它的空窗口版本 —— 否则同一号码会
        // 先后播 "请GA170到号窗口办理"(半截话) 和 "请GA170到C06号窗口办理"。
        // 详见 drop_shadow_empty() 的注释。
        Ok((raws.join(" "), drop_shadow_empty(cands)))
    }

    /// 单行裁剪图的识别。
    ///
    /// 两条链路(2026-10-27 按行分流):
    ///   - 普通行(深蓝底白字): 原图 -> 旧增强 psm(主) -> 旧增强 psm(副)
    ///   - 高亮行(浅蓝底白字): 新增强 psm(主) -> 新增强 psm(副) -> 原图兜底
    ///
    /// ★高亮行为什么优先专用增强: 原图对高亮行常给出**号对窗错**的假阳性
    ///   (实测 `到C0 而力有`、`到G05号窗日`)。原图一旦"解析出候选"就不会再走
    ///   兜底, 于是假窗口被当成真值播出去 —— 比读不出更糟。所以优先专用增强；
    ///   增强只读到号码时允许原图仅补完整窗口，两档都空时才无条件回退原图。
    /// ★旧增强在高亮行上是负收益(浅底被判成字, 61% 像素连成黑块), 所以高亮行
    ///   完全不走旧链。
    fn recognize_one(
        &self,
        crop: &image::RgbImage,
        highlight: bool,
    ) -> Result<(String, HashSet<(String, String)>), String> {
        let psm_main = self.ocr.default_psm();
        // 兜底第二档必须与主档**不同**: 配置 psm=6 时旧代码硬编码第二档也是 6,
        // 同一张图同一参数跑两遍, 第二遍永远不可能带来新信息(纯浪费 1.5s)。
        let psm_alt = if psm_main == 6 { 11 } else { 6 };
        // 大图不做增强(CPU 保护): 宽度超 1280 或 ROI 面积超预算时, 增强的 2x 放大
        // 加上 31x31 局部均值会让预处理在 tesseract 超时保护之外长时间占住主循环。
        let enhance_ok = enhancement_pixels_allowed(self.roi.2, self.roi.3);

        if highlight && enhance_ok {
            let up = detect::enhance_highlight_for_ocr(crop);
            let mut fallback_raw = String::new();
            let mut best: Option<RankedOcrResult> = None;
            for psm in [psm_main, psm_alt] {
                if let Ok(raw2) = self.ocr.recognize_psm(&up, psm) {
                    let c2 = self.ocr.parse(&raw2);
                    if fallback_raw.is_empty() {
                        fallback_raw = raw2.clone();
                    }
                    let q = ocr_candidate_quality(&c2);
                    if q > best.as_ref().map_or(0, |(_, _, old_q)| *old_q) {
                        best = Some((raw2, c2, q));
                    }
                    if q == 2 {
                        break;
                    }
                }
            }
            if let Some((best_raw, best_cands, best_q)) = best {
                if best_q == 2 {
                    return Ok((best_raw, best_cands));
                }
                // 增强图只读到号码时，允许原图补完整窗口 —— 但**号码锁死增强图, 只借窗口**。
                // ★2026-10-28 改: 旧实现按"完整度"整组替换(`原图q > 增强q` 就 return 原图),
                //   原图把 GA013 读成 GAO013 的错号会盖掉增强图已经读对的号。判据见
                //   fill_window_keep_number()。
                //
                // ★原图这一档用 Ok 判定、不用 `?`: tesseract 超时/管道错误在现场是常态,
                //   一次超时不该把增强图已经读到的号码连带丢掉(第 1 行时还会让整轮识别报错)。
                if let Ok(raw) = self.ocr.recognize(crop) {
                    let donor = self.ocr.parse(&raw);
                    let (merged, filled, rejected) = fill_window_keep_number(&best_cands, &donor);
                    // 只在"真补到窗口"或"原图给了不同号码"时打一行: 两种情况都少见,
                    // 但正是复盘 4 位数误播时最想看到的证据(增强图 vs 原图各读了什么)。
                    if !filled.is_empty() || !rejected.is_empty() {
                        println!(
                            "[识别] 高亮行原图补窗口: 补={:?} 拒收号码={:?} 增强图原文=[{}] 原图原文=[{}]",
                            filled,
                            rejected,
                            short(&best_raw),
                            short(&raw)
                        );
                    }
                    return Ok((best_raw, merged));
                }
                return Ok((best_raw, best_cands));
            }
            // 专用增强全空: 退回原图再试一次。此刻原图大概率也读不出, 但只要
            // 能读出就是净收益, 读不出也只是回到"这行没认到"。
            let raw = self.ocr.recognize(crop)?;
            let cands = self.ocr.parse(&raw);
            // 原图也没读出字: 把增强图的乱码留在 raw 里, 日志能看出"这行不是没看,
            // 是看了读不成", 排查时与"根本没跑 OCR"区分得开。
            let raw = if raw.trim().is_empty() {
                fallback_raw
            } else {
                raw
            };
            return Ok((raw, cands));
        }

        let raw = self.ocr.recognize(crop)?;
        let cands = self.ocr.parse(&raw);
        let mut best_raw = raw;
        let mut best_cands = cands;
        let mut best_q = ocr_candidate_quality(&best_cands);
        if best_q < 2 && enhance_ok {
            // 兜底增强识别链: 原图(上面已跑) -> 增强图 psm(主) -> 增强图 psm(副)。
            // ★为什么要增强: 真实摄像头画面是"白字深底+小字+糊", tesseract 对
            //   原图几乎全乱码(靶机实测 [播报] 长期为 0 的根因之一), 沙盒用干净
            //   测试卡测不出来。2x放大+红通道+自动对比度+反相后靶机实测能把
            //   "GA777到C03号窗口/GA999到C01号窗口"读出来。两档 psm 错误模式不同
            //   (实测同一图一个读对窗口、一个读对号码), 各试一次先中先用。
            let up = detect::enhance_for_ocr(crop);
            for psm in [psm_main, psm_alt] {
                if best_q == 2 {
                    break;
                }
                if let Ok(raw2) = self.ocr.recognize_psm(&up, psm) {
                    let c2 = self.ocr.parse(&raw2);
                    let q = ocr_candidate_quality(&c2);
                    if q > best_q {
                        best_q = q;
                        best_cands = c2;
                        best_raw = raw2;
                    }
                }
            }
        }
        Ok((best_raw, best_cands))
    }

    /// 把本轮识别结果归账: 诊断计数 -> 确认窗口/退避 -> 预热 -> 确认状态机 -> 票决播报。
    fn ingest_candidates(
        &mut self,
        cands: HashSet<(String, String)>,
        raw: &str,
        iter_secs: f64,
        by_change: bool,
        now: f64,
    ) {
        self.note_ocr_cost(iter_secs);
        // ★号码闸门(2026-10-21): 先把"老号/跳号"滤掉 —— 三行列表里就只剩最新那个。
        // 另外两个判断(空转退避、last_read_ok)必须用**过滤前**的 raw_has:
        // 屏幕上只有老号时, 说明读得很清楚, 不该被当成"没读到"去翻倍兜底间隔,
        // 否则下一个新号要靠兜底轮才发现, 反应会被拖慢到 30s。
        let raw_has = !cands.is_empty();
        let (cands, pool_rejected) = filter_number_pool(cands, self.number_pool_max);
        if by_change && !pool_rejected.is_empty() {
            println!(
                "[闸门] 号码池上限 {} 拒绝候选: {}",
                self.number_pool_max,
                pool_rejected.join(", ")
            );
            for number in &pool_rejected {
                logger::log(&self.cfg.log_file, number, "", raw, "INVALID");
            }
        }
        let (cands, jump_blocked) = self.gate_filter(cands, now);
        // ★确认窗口自适应: OCR 一步就要 1.5~10s 时, 固定 4s 的 TTL 会在
        //   下一轮到来前把候选清掉 -> 永远凑不齐 stable_frames 轮 -> 不播报。
        //   (靶机实测就是这样死的: 圈速 24s > TTL 4s。)
        //   有效 TTL = max(配置值, 3×本轮OCR耗时), 静止的号不会被误清。
        // ★死锁防线(2026-10-22 现场): 上面那一项只覆盖"OCR 本身慢", 覆盖不了
        //   "空转退避把兜底间隔拉到 30s"。过曝/条纹帧下 tesseract 是 0.1s 就
        //   正常退出 + 空输出(不算失败), empty_streak 一路涨到 4 → 兜底间隔 30s,
        //   而 TTL 按 3×0.1s 只算出 4s → 下一轮开头 retain 就把候选连计数清掉,
        //   确认数永远是 1, 静默不播(现场 16:14~16:54 就是这个死法)。
        //   退避的语义是"少跑几次 OCR", 不该连带惩罚已经确认过的候选,
        //   所以把"本轮实际等待的间隔"和"下一轮将要等待的间隔"都计入 TTL 下界。
        let fb_prev = self.fb_eff;
        // 空转退避: 连续无候选就把兜底间隔翻倍(上限 30s), 有候选立刻复位。
        if !raw_has {
            self.empty_streak = self.empty_streak.saturating_add(1);
            let k = 1u32 << self.empty_streak.min(4);
            self.fb_eff = (self.fb_sec * k as f64).min(30.0);
        } else {
            self.empty_streak = 0;
            self.fb_eff = self.fb_sec;
        }
        let ttl_eff = effective_ttl(self.ttl, iter_secs, fb_prev, self.fb_eff);
        // TTS 预热: 第一轮看到候选就后台合成(结果落在常驻服务缓存里),
        // 等第 stable_frames 轮确认通过再播时即命中缓存(~毫秒),
        // 相当于把 1.8s 的合成耗时从关键路径上整体藏掉。
        self.last_read_ok = raw_has;
        for cand in &cands {
            self.cand_raw.insert(cand.clone(), raw.to_string(), now);
            // 预热按【号码】判重, 与播报去重键保持一致。
            if !self.announced.contains_key(&cand.0) && self.prefetched.insert(cand.clone()) {
                if self.prefetched.len() > 64 {
                    self.prefetched.clear();
                }
                self.ann.prefetch(&cand.0, &cand.1);
            }
        }
        // 只对"尚未播报"的候选续观察期。若对已播报的号码也续期,
        // 屏幕一直显示号码时观察期会被无限续期 -> OCR 变相常开白烧 CPU。
        if cands.iter().any(|c| !self.announced.contains_key(&c.0)) {
            self.active_until = now + self.active_sec;
        }
        // 有跳号嫌疑被拦 -> 同样保持观察期, 让下一轮 OCR 尽快重认。
        // 误读在下一轮多半就变对了; 真跳号连续被拦超过 GATE_FORCE_SECS 会强制放行。
        if jump_blocked {
            self.active_until = now + self.active_sec;
        }
        // 清理超时未再出现的候选(闪变残留), 不立即清零 -> 短暂被替换的号码仍保留计数
        self.confirm.retain(|_, v| now - v.1 <= ttl_eff);
        let mut sorted: Vec<(String, String)> = cands.iter().cloned().collect();
        sorted.sort();
        for cand in sorted {
            let (cnt, first) = match self.confirm.get(&cand) {
                Some(&(c, _, f)) => (c + 1, f),
                None => (1, now), // 首次出现时刻 = 真正的"屏幕变化"起点
            };
            self.confirm.insert(cand.clone(), (cnt, now, first));
        }
        self.announce_winners(raw, now, iter_secs);
        // 只对"因变化而跑、且真的什么都没解析出来"的那一轮记 INVALID:
        // 兜底轮读到空属正常; 屏幕只有老号(被闸门滤掉)也不该记 INVALID。
        if !raw_has && by_change {
            logger::log(&self.cfg.log_file, "", "", raw, "INVALID");
        }
    }

    /// 单个号码是否"够格播"(纯判定, 不改状态)。
    /// ★2026-10-23 起不再被提前退出使用(条件已反转为"所见皆已播"),
    /// 保留作闸门语义的文档与未来调用点, 抑制 dead_code 告警。
    #[allow(dead_code)]
    fn gate_accepts(&self, number: &str) -> bool {
        if !number_pool_accepts(number, self.number_pool_max) {
            return false;
        }
        if self.max_jump == 0 {
            return true;
        }
        let (pre, v) = match split_num(number) {
            Some(x) => x,
            None => return true,
        };
        match self.last_num.get(&pre) {
            None => true,
            // 硬上限放到 JUMP_HARD_MAX: 段内靠"多要确认量"分级把关, 不再一刀切。
            Some(&last) => v > last && v <= last + JUMP_HARD_MAX,
        }
    }

    /// 该号码在"当前基线"下需要额外几个确认量(静帧 tick / 投票轮)。
    /// 基线未知或号码不参与闸门时返回 0 —— 退化成原来的行为, 不会卡住。
    fn jump_extra(&self, number: &str) -> u32 {
        if self.max_jump == 0 {
            return 0;
        }
        let (pre, v) = match split_num(number) {
            Some(x) => x,
            None => return 0,
        };
        match self.last_num.get(&pre) {
            None => 0,
            Some(&last) if v > last => jump_extra_confirm(v - last, self.max_jump),
            _ => 0,
        }
    }

    /// 按下述规则过滤候选(多行列表下同时承担"挑最新号")。
    /// 返回 `(放行的候选, 是否有跳号被拦)` —— 后者用来让主循环保持快速重读,
    /// 因为被拦的多半是误读, 下一轮 OCR 往往就变对了; 真跳号则靠静默兜底放行。
    fn gate_filter(
        &mut self,
        cands: HashSet<(String, String)>,
        now: f64,
    ) -> (HashSet<(String, String)>, bool) {
        if self.max_jump == 0 || cands.is_empty() {
            return (cands, false);
        }
        let mut by_pre: HashMap<String, Vec<(String, String, u32)>> = HashMap::new();
        let mut out: HashSet<(String, String)> = HashSet::new();
        // 本轮看见的全部候选(闸门过滤前), 供序列重置的"连续性"判定用。
        let cand_all: HashSet<(String, String)> = cands.clone();
        for (n, w) in cands {
            match split_num(&n) {
                Some((pre, v)) => by_pre.entry(pre).or_default().push((n, w, v)),
                // 拆不出数字段的候选(理论上不会出现)不参与闸门, 原样放行
                None => {
                    out.insert((n, w));
                }
            }
        }
        let silent = now - self.last_announce;
        let mut blocked: Vec<String> = Vec::new();
        for (pre, items) in by_pre {
            match self.last_num.get(&pre).copied() {
                // 无基线(刚部署/跨天): 只取最大号 —— 三行列表里最新的一行在最上面,
                // 号码又是递增的, 所以"最大"就是"最新"。
                None => {
                    if let Some(mx) = items.iter().map(|x| x.2).max() {
                        for (n, w, v) in &items {
                            if *v == mx {
                                out.insert((n.clone(), w.clone()));
                            }
                        }
                    }
                }
                Some(last) => {
                    let mut any = false;
                    for (n, w, v) in &items {
                        // 放行区间: (上次号, 上次号 + JUMP_HARD_MAX] —— 老号(≤基线)与
                        // 离谱跳号(>硬上限, 基本是误读)都挡; 中间段靠分档确认把关。
                        if *v > last && *v <= last + JUMP_HARD_MAX {
                            out.insert((n.clone(), w.clone()));
                            any = true;
                        } else if *v > last {
                            blocked.push(format!("{}(+{})", n, *v - last));
                        } else if *v + BACKFILL_MAX > last && !self.announced.contains_key(n) {
                            // ★漏号补播(2026-10-23 现场 GA126 事故): 见 BACKFILL_MAX 注释。
                            // 紧贴基线下方 + 当日从未播过才放行; 已播过的屏上残留
                            // 被开机 seed 的当日去重表挡住, 不会重播。
                            out.insert((n.clone(), w.clone()));
                            any = true;
                        } else if seq_reset_eligible(true, last, *v) {
                            // ★序列重置(2026-10-26 现场 GA174→GA193 事故): 远低于基线
                            // (差 > BACKFILL_MAX)的号 —— 旧逻辑一律当"老号"挡死, 于是
                            // 一旦基线被误读带偏, 当天剩余的真小号全哑 (175~178 就是这么
                            // 丢的)。这里给一个出口: 该号在**同一静帧段内连续稳定
                            // SEQ_RESET_SECS 秒**, 判定为新一轮/回退, 重置基线并放行
                            // (播报该号, 它就是屏幕当前真实叫号)。
                            // 不误判残影的原因见 SEQ_RESET_SECS 注释。
                            let key = (n.clone(), w.clone());
                            let since = self.seq_since.get(&key).copied();
                            let (start, fire) = seq_reset_tick(since, now);
                            if fire {
                                self.last_num.insert(pre.clone(), *v);
                                self.seq_since.clear();
                                println!(
                                    "[闸门] 序列重置: {} 静帧内稳定 {:.0}s(阈值 {:.0}s)且远低于基线 {} \
                                     -> 判为新一轮, 基线重置为 {} 并放行",
                                    n, now - start, SEQ_RESET_SECS, last, v
                                );
                                out.insert(key);
                                any = true;
                            } else {
                                self.seq_since.insert(key, start);
                                blocked.push(format!(
                                    "{}(<{}, 序列重置 {:.0}/{:.0}s)",
                                    n,
                                    last,
                                    now - start,
                                    SEQ_RESET_SECS
                                ));
                            }
                        }
                    }
                    // 兜底: 长时间一句没播, 而又有"更大的号"被硬上限挡住
                    // (多半是基线被某次误读带偏了) -> 强制放行最大的那个, 防整天静默。
                    if !any && silent >= GATE_FORCE_SECS {
                        if let Some(mx) = items.iter().filter(|x| x.2 > last).max_by_key(|x| x.2) {
                            println!(
                                "[闸门] 已静默 {:.0}s, {} 超出硬上限 +{} -> 强制放行(基线 {} 可能被误读带偏)",
                                silent, mx.0, JUMP_HARD_MAX, last
                            );
                            out.insert((mx.0.clone(), mx.1.clone()));
                        }
                    }
                }
            }
        }
        // 静帧段变更: 屏上的画面又动过(换号动画/刷新), 整表作废重新起算 ——
        // 这是"残影攒不满"的核心保证: 残影活不过换号动画, 动画一结束段就换了。
        // 放在放行判定之后: 本轮真触发重置时表已被整体清空, 这里不再有影响。
        if (self.seq_seg - self.static_since).abs() > f64::EPSILON {
            self.seq_seg = self.static_since;
            self.seq_since.clear();
        }
        // 同一静帧段内也要"本轮没再看见就归零": 防止某号被读了两次、中间隔了几轮
        // 没读到时把首尾时间差当成"连续稳定"。
        let seen = cand_all;
        self.seq_since.retain(|k, _| seen.contains(k));
        if !blocked.is_empty() && now - self.gate_log_at >= 10.0 {
            self.gate_log_at = now;
            println!(
                "[闸门] 拦截 {} (上限 +{}, 当前基线 {:?})",
                blocked.join(", "),
                self.max_jump,
                self.last_num
            );
        }
        (out, !blocked.is_empty())
    }

    /// 记一次成功播报, 推进闸门基线。
    fn note_announced(&mut self, number: &str) {
        if let Some((pre, v)) = split_num(number) {
            // ★基线不回退: 补播漏号(GA126 在 GA127 之后才播)会把基线往回拉,
            // 补播窗口 (last-5, last] 随之重新拉开, 可能引发连环补播。
            // 基线始终取该前缀的历史最大; 补播号只进去重表防重播。
            self.last_num
                .entry(pre)
                .and_modify(|e| {
                    if v > *e {
                        *e = v;
                    }
                })
                .or_insert(v);
        }
    }

    /// ★2026-10-23 中危修复(第八轮): 跨日重置后的"首个无基线候选"只标记不播报。
    ///
    /// 场景: 24h 连跑, 零点 maintenance 清空 last_num, 屏上却停着昨日尾号 GA352。
    /// 闸门"无基线取最大"会把它放行进播报 —— 零点误播, 且 note_announced 把基线
    /// 毒化成 352, 次日 GA001 起全天被 (v>last 才放行) 静默吞掉(强放兜底也只放
    /// v>last, 救不回小号)。
    ///
    /// 处理: 只进当日去重表(后续轮次被 announced.contains_key 压住), 不 speak、
    /// 不写 OK 日志(写了会被 seed_last_numbers 当基线捡回去)、不动 last_num ——
    /// 基线留给次日第一个真号(GA001 走"无基线取最大"正常播报并播种)。
    /// 与 seed_last_numbers "只播种不播报"语义对齐: 播种的目的都是防误播,
    /// 而非替号码"认领"播报资格。
    ///
    /// 返回 true 表示已按"只标记"处理, 调用方应跳过后续播报动作。
    fn seed_only_after_reset(&mut self, number: &str, now: f64) -> bool {
        let has_baseline = split_num(number)
            .map(|(pre, _)| self.last_num.contains_key(&pre))
            .unwrap_or(false);
        if !seed_only_decide(self.day_reseed, has_baseline) {
            return false;
        }
        self.announced.insert(number.to_string(), (), now);
        println!(
            "[闸门] 跨日重置: 候选 {} 只标记不播报(屏上昨日残留, 防零点误播与基线毒化)",
            number
        );
        true
    }

    /// 按【号码】票决后播报: 票数达到 need_stable 且该号码未播报过才入队。
    /// 聚合口径见 number_votes() —— 同一号码的多个窗口候选并成一票, 只播一次。
    fn announce_winners(&mut self, raw: &str, now: f64, iter_secs: f64) {
        // ★作息闸(2026-10-22 中危, 2026-10-23 收敛到统一入口): schedule.enabled
        //   且当前不在工作时段 -> 不播。OCR 链照常跑(确认/日志/诊断不受影响),
        //   回到时段自动续播 —— "夜里重启主机服务整夜播报"的程序侧防线, 与
        //   systemd worktime timer 互为兜底。放在 announced.insert 之前: 静音期
        //   号码不占当日去重表, 恢复后屏幕上的现有号还能正常报出来。
        if !self.announcement_allowed() {
            return;
        }
        let mut winners = number_votes(&self.confirm);
        // 本轮是否"真的处理过至少一个候选"(标记或播报)。见末尾 day_reseed 清理条件:
        // 读空的一轮不算"首批已处理", 否则标记会被白白清掉。
        let mut handled = false;
        // 同 fast_confirm: 数值序播报(GA9 先于 GA10), 不用字符串序(第八轮低危)。
        winners.sort_by_key(|a| announce_sort_key(&a.0, &a.1));
        for (number, window, votes, first, _last) in winners {
            // 跳幅大 -> 多要几票(每票≈一轮 OCR), 防过渡帧/反光残影把大号送出去。
            let extra = self.jump_extra(&number);
            if votes < self.need_stable + extra || self.announced.contains_key(&number) {
                continue;
            }
            // 能走到这里说明该号够票且未播过 —— 本轮确实处理了它(下面是标记或播报)。
            handled = true;
            if self.seed_only_after_reset(&number, now) {
                continue;
            }
            self.announced.insert(number.clone(), (), now);
            self.note_announced(&number);
            logger::log(&self.cfg.log_file, &number, &window, raw, "OK");
            self.ann.speak(&number, &window); // 入FIFO队列, 播报线程按顺序播
            self.last_announce = now; // 健康心跳: 真有号播了, 复位闲置计时
            println!(
                "[时序] 首见→开始播报 {:.0} ms (确认{}轮/需{}, 跳幅多确认+{}, 本轮OCR {:.1}s); 另加相机/网络/轮询约 0.3~0.8 s",
                (now - first) * 1000.0,
                votes,
                self.need_stable + extra,
                extra,
                iter_secs
            );
        }
        // 同 fast_confirm: 首批处理完清跨日标记, 去重表接管残留号的后续压制。
        // ★2026-10-26(改动二)修正: 只在"本轮真的处理过至少一个候选"时才清 ——
        //   开机第一轮 OCR 常常读空(屏刚上电闪黑/未聚焦/换号动画中), 若照样清标记,
        //   下一轮那个残留大号就会趁虚而入把基线毒化, 整个补丁等于白打。没处理候选
        //   就保持标记, 等真正出现候选那一轮再判定"只标记 / 正常播"。
        if !keep_day_reseed(handled) {
            self.day_reseed = false;
        }
    }

    /// 记一轮 OCR 的耗时进诊断窗口(诊断心跳的 [监测] 行用)。
    fn note_ocr_cost(&mut self, secs: f64) {
        self.diag_ocr_n += 1;
        self.diag_ocr_total += secs;
        if secs > self.diag_ocr_max {
            self.diag_ocr_max = secs;
        }
    }

    /// OCR 失败(超时/子进程非零退出)的归账: 同样计入诊断与空转退避, 别让病态帧
    /// 把兜底间隔钉在基础值上; 同时记下"内容未知", 下一轮兜底不放宽。
    fn note_ocr_failure(&mut self, e: &str, iter_secs: f64) {
        self.note_ocr_cost(iter_secs);
        self.empty_streak = self.empty_streak.saturating_add(1);
        let k = 1u32 << self.empty_streak.min(4);
        self.fb_eff = (self.fb_sec * k as f64).min(30.0);
        self.last_read_ok = false;
        println!("[OCR失败] {}", e);
    }

    /// 健康心跳: 距上次成功播报超过阈值就打一条 `[健康]` 告警。
    ///
    /// 为什么需要它(评审 P2-7): 本程序最危险的失效不是崩溃 —— 崩溃有 systemd 拉起。
    /// 最危险的是"进程活着、链路悄悄死了": 摄像头黑屏 / 失焦 / 被拔掉之后, 帧差恒为 0、
    /// OCR 永远读不出内容, 主循环一切"正常", 现场却一整天没播报且无人知晓。
    /// 心跳把这种静默软故障变成一条可见的日志。
    ///
    /// 限流: 每过 `health_alert_sec` 最多打一条, 免得 60s 的维护节拍把 journal 刷满。
    fn health_check(&mut self, now: f64) {
        if self.health_alert_sec <= 0.0 {
            return; // 显式关闭(config: health_alert_min = 0)
        }
        // ★2026-10-23 中危修复: 作息静音期 last_announce 停更是设计行为, 不是
        //   链路失效 —— 不豁免的话静音期整夜每 health_alert_sec 刷一条误导告警。
        if self.cfg.schedule.enabled {
            // ★2026-10-31: 非工作日整天不播是设计行为, 同属"静音期", 一样要豁免,
            //   否则周末每 health_alert_sec 刷一条"链路疑似失效"的误导告警。
            if !self.workday_active_now() {
                return;
            }
            let hm = chrono::Local::now().format("%H:%M").to_string();
            if !in_work_window(
                &hm,
                &self.cfg.schedule.work_start,
                &self.cfg.schedule.work_end,
            ) {
                return;
            }
        }
        let idle = now - self.last_announce;
        if idle >= self.health_alert_sec && now - self.last_health_warn >= self.health_alert_sec {
            self.last_health_warn = now;
            println!(
                "[健康] 距上次成功播报已 {:.1} 分钟, 进程存活但链路疑似失效 —— \
                 请检查取流(黑屏/失焦/断线)与 OCR; 如需关闭本告警把 health_alert_min 设为 0",
                idle / 60.0
            );
        }
    }

    /// 维护节拍: ① 清掉过期的"已播报"记录(旧实现从不清理 -> 跨天后同一号码永久哑掉);
    /// ② 日志写满阈值就归档(旧实现只管写, 写满 SD 卡只能等部署层第二天来收);
    /// ③ 跨日重置闸门基线 —— 去重表每日零点清零(dedupe.rs DailyMidnight), last_num
    ///    却只在启动时 seed, 7×24 连跑时昨天的基线会把今天从头递增的低号码整段
    ///    静默吞掉(gate 只放行 (基线, 基线+硬上限]), 与去重表当年如出一辙。
    /// 清理/轮转/重置都打日志: 旧实现的"静默软故障"正是因为它悄悄什么都不做 ——
    /// 让隐式维护显形, 现场看一眼 journal 就知道治理在不在工作。
    fn maintenance(&mut self, now: f64) {
        if now - self.last_maint < self.maint_every {
            return;
        }
        self.last_maint = now;
        if gate_day_rollover(&mut self.maint_day, local_day_of(now)) && !self.last_num.is_empty() {
            println!(
                "[闸门] 跨日重置号码基线 {:?} (去重表已按日清零, 基线跟随)",
                self.last_num
            );
            self.last_num.clear();
            // ★2026-10-23 中危修复(第八轮): 置"等待首个候选"标记 —— 屏上若还停着
            // 昨日尾号, 首个无基线候选只标记不播报(见 seed_only_after_reset)。
            self.day_reseed = true;
        }
        let n_ann = self.announced.prune(now);
        let n_raw = self.cand_raw.prune(now);
        if n_ann > 0 || n_raw > 0 {
            println!(
                "[维护] 去重表清理: 已播报 {} 条 / 候选原文 {} 条; 现存 {}/{}",
                n_ann,
                n_raw,
                self.announced.count(),
                self.cand_raw.count()
            );
        }
        // 归档目录配了空串 -> 回落到与 deploy/announce-cleanup.sh 一致的默认目录,
        // 免得"配错一个空值"让日志治理悄悄失效。
        let dir = if self.cfg.log_archive_dir.trim().is_empty() {
            logger::DEFAULT_ARCHIVE_DIR
        } else {
            self.cfg.log_archive_dir.trim()
        };
        if let Some(p) = logger::rotate_if_needed(
            &self.cfg.log_file,
            dir,
            self.log_max_bytes,
            self.cfg.log_keep,
        ) {
            println!("[日志] 已达阈值, 轮转 -> {}", p);
        }
    }
}

/// 按【号码】聚合确认票, 返回 `(号码, 最佳窗口, 总票数, 最早首见, 最晚最后出现)`。
///
/// 为什么按号码而不是"号码+窗口"(2026-10-13 现场实测):
///   同一个 GA174 一天被播了 4 遍 —— `GA174/C04`、`AT174/C04`(过渡帧把 G 读成 AT)、
///   `GA174/CC04`、`GA174/C045`(窗口误读)。旧实现用"号码+窗口"当去重键与投票键,
///   每个误读结果都是**新键**, 于是每来一个新误读就当新叫号播一遍。
///   号码才是"这一次叫号"的稳定标识, 窗口只是附属信息。
///
/// 窗口取"票数最多的那条候选"(并列取最近出现的), 避免误读窗口压过正确窗口。
/// 抽成自由函数(只读 confirm 表)是为了可单测 —— 这是现场调优的核心逻辑。
fn number_votes(
    confirm: &HashMap<(String, String), (u32, f64, f64)>,
) -> Vec<(String, String, u32, f64, f64)> {
    // 号码 -> (最佳窗口, 总票数, 最佳窗口票数, 最佳窗口最后时刻, 号码最晚出现时刻)
    let mut agg: HashMap<String, (String, u32, u32, f64, f64)> = HashMap::new();
    for ((n, w), &(cnt, last, _first)) in confirm.iter() {
        let e = agg
            .entry(n.clone())
            .or_insert((w.clone(), 0, 0, f64::NEG_INFINITY, last));
        e.1 += cnt;
        if cnt > e.2 || (cnt == e.2 && last >= e.3) {
            e.0 = w.clone();
            e.2 = cnt;
            e.3 = last;
        }
        if last > e.4 {
            e.4 = last;
        }
    }
    // 顺带把每个号码的"最早首见"补上(用于 [时序] 延时统计)
    let mut firsts: HashMap<String, f64> = HashMap::new();
    for ((n, _), &(_, _, first)) in confirm.iter() {
        let e = firsts.entry(n.clone()).or_insert(first);
        if first < *e {
            *e = first;
        }
    }
    agg.into_iter()
        .map(|(n, (w, tot, _, _, last))| {
            let f = firsts.get(&n).copied().unwrap_or(last);
            (n, w, tot, f, last)
        })
        .collect()
}

/// 丢弃"同一号码另有带窗口候选"的空窗口候选。
///
/// 起因(2026-10-13 现场实测): OCR 配了两条正则 —— pattern1 只匹配号码
/// (`([A-Z]{1,2}\d{3,6})`), pattern2 匹配"号码+窗口"。屏幕正常时两者同时命中,
/// 于是同一个号码会产生 (GA170, "") 与 (GA170, "C06") 两个候选, 播报线程按序号
/// 依次播放 —— 先来一句"请GA170到号窗口办理"(窗口为空, 半截话), 再播完整的那句。
/// 现场听感就是"每叫一个号都先嘟噜一句废话"。
///
/// 只丢弃"确实存在同号码带窗口候选"的那条空窗口候选: 号码本身若没有带窗口版本
/// (屏幕上只显示号码没显示窗口), 空窗口候选照常保留, 不改变原有行为。
fn drop_shadow_empty(cands: HashSet<(String, String)>) -> HashSet<(String, String)> {
    let with_win: HashSet<String> = cands
        .iter()
        .filter(|(_, w)| !w.is_empty())
        .map(|(n, _)| n.clone())
        .collect();
    if with_win.is_empty() {
        return cands;
    }
    cands
        .into_iter()
        .filter(|(n, w)| !(w.is_empty() && with_win.contains(n)))
        .collect()
}

/// 日志里 OCR 原文的截断长度。现场一行原文常带一长串噪声字符,
/// 全打会把 service.log 冲爆, 80 字符足够看出"读成了什么"。
const SHORT_MAX: usize = 80;

/// 把 OCR 原文压成一行短串, 只用于打日志。
fn short(s: &str) -> String {
    let t: String = s
        .trim()
        .chars()
        .filter(|c| *c != '\r')
        .map(|c| if c == '\n' { ' ' } else { c })
        .collect();
    let mut out: String = t.chars().take(SHORT_MAX).collect();
    if t.chars().count() > SHORT_MAX {
        out.push('…');
    }
    out
}

/// `fill_window_keep_number` 的返回值: (合并后的候选, 补上的窗口, 被拒收的原图号码)。
/// 后两项只用于打日志 —— 抽成别名是为了过 `clippy::type_complexity`。
type WindowFill = (
    HashSet<(String, String)>,
    Vec<(String, String)>,
    Vec<String>,
);

/// 高亮行"原图补窗口"的采纳规则: **号码锁死增强图, 只从原图借窗口**。
///
/// 起因(2026-10-28 复盘 4 位数误播): 旧实现按 `ocr_candidate_quality` 择优 ——
/// 只看"有没有窗口", 不看号码是否一致。增强图只读到号码(q=1)、原图恰好读到
/// "号+窗口"(q=2) 时整组替换, 于是原图把 `GA013` 读成 `GAO013`(旧解析拼成
/// `GA0013`) 的错号, 会盖掉增强图已经读对的 `GA013`。原注释只防了"号对窗错"
/// 的假窗口, 没防"错号盖对号"。
///
/// 判据: 原图给出的号码与增强图**完全一致**才借它的窗口; 号码不同一律拒收。
/// 宁可播"请GA013到号窗口办理"这半句, 也不播一个不存在的号 —— 窗口读错只是
/// 引导到错窗口(现场还能问), 号码读错是叫错人。
///
/// 返回 (合并后的候选, 补上的窗口, 被拒收的原图号码), 后两项只用于打日志。
fn fill_window_keep_number(
    kept: &HashSet<(String, String)>,
    donor: &HashSet<(String, String)>,
) -> WindowFill {
    // 原图里 "号码 -> 第一个非空窗口"(同一号码理论上只有一个窗口, 取任意一个即可)
    let donor_win: HashMap<&str, &str> = {
        let mut m: HashMap<&str, &str> = HashMap::new();
        for (n, w) in donor {
            if !w.is_empty() {
                m.entry(n.as_str()).or_insert(w.as_str());
            }
        }
        m
    };

    let mut out: HashSet<(String, String)> = HashSet::with_capacity(kept.len());
    let mut filled: Vec<(String, String)> = Vec::new();
    for (n, w) in kept {
        if !w.is_empty() {
            out.insert((n.clone(), w.clone())); // 增强图自己有窗口, 原图无权改
            continue;
        }
        match donor_win.get(n.as_str()) {
            Some(dw) => {
                filled.push((n.clone(), (*dw).to_string()));
                out.insert((n.clone(), (*dw).to_string()));
            }
            None => {
                out.insert((n.clone(), w.clone()));
            }
        }
    }

    // 原图读到、但增强图完全没有的号码: 一个都不采纳, 只记录供现场排查。
    let mut rejected: Vec<String> = donor
        .iter()
        .map(|(n, _)| n)
        .filter(|n| !kept.iter().any(|(k, _)| k == *n))
        .cloned()
        .collect();
    rejected.sort();
    rejected.dedup();
    filled.sort();

    (out, filled, rejected)
}

/// 清理 `_live/` 里"确定是上一轮残留"的临时文件: 预览原子写留下的半截 `.tmp`、
/// 以及没人消费的预览请求 `req`。
///
/// **故意不动 `trig_*.jpg`**: 那是触发时刻的存证帧, 是现场排障最想看的东西 ——
/// 启动就删等于把上一轮出问题时的证据抹掉。它们的保留期由部署层
/// `deploy/announce-cleanup.sh` 按 7 天统一管理。
fn cleanup_live_dir() {
    let rd = match std::fs::read_dir(SAVE_DIR) {
        Ok(r) => r,
        Err(_) => return, // 目录还不存在, 没什么可清
    };
    let mut n = 0u32;
    for e in rd.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        let junk = name == "latest.jpg.tmp" || name == "req";
        if junk && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    if n > 0 {
        println!("[启动] 清理 _live/ 上次遗留的临时文件 {} 个", n);
    }
}

/// 启动期: 读配置 -> 抢单实例锁 -> 装信号 -> 清残留 -> 自检外部依赖 -> 进主循环。
fn run() -> Result<(), String> {
    let cfg = Config::load("config.json")?;
    // 锁端口可配置: 便于不打扰生产实例的前提下另起模拟实例自测
    let _lock = acquire_lock(cfg.lock_port)?;

    // ★优雅退出第一件事: 先把信号处理装好, 再干别的。
    //   顺序不能反 —— 装之前收到的 SIGTERM 会直接终止进程, 那样 Loop 里的
    //   PipeGrabber 不会被析构, ffmpeg 子进程就留成占住 /dev/video0 的孤儿了。
    signal::install();
    // 上次若是被 kill -9 / 掉电打断的, _live/ 会留下半截 .tmp 和没人消费的 req。
    // 只清这两类"确定是垃圾"的, **不动 trig_*.jpg** —— 那是上一轮的现场证据。
    cleanup_live_dir();
    // 启动自检: 把 ffmpeg / tesseract 的版本或缺失写进日志(P2-5)。
    // 只做记录、不阻断启动 —— "换个靶机就变慢/识别全空"这类问题一眼可对。
    selfcheck::report();

    // _lock 在本函数作用域内, Loop 是临时值: 析构顺序 = Loop(收 ffmpeg) -> _lock,
    // 与旧实现(run 内先声明 _lock 后声明 src)一致。
    Loop::build(cfg)?.run();
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("[退出] {}", e);
        std::process::exit(1);
    }
    // 正常路径(run 返回 Ok)只可能是收到停止信号后 break —— 退出码 0 表示
    // "优雅停机", systemd 看到 Result=success 就不会记为失败。
}

#[cfg(test)]
mod tests {
    use super::{
        announce_sort_key, drop_shadow_empty, effective_ttl, enhancement_pixels_allowed,
        fill_window_keep_number, filter_number_pool, gate_day_rollover, in_work_window,
        jump_extra_confirm, keep_day_reseed, number_pool_accepts, number_votes,
        ocr_candidate_quality, resolve_highlight_row, resolve_row_pitch, schedule_day_active,
        seed_only_decide, seed_only_on_startup, seq_reset_eligible, seq_reset_tick, settle_ready,
        short, should_stop_before_row, time_plausible_y, JUMP_HARD_MAX, SEQ_RESET_SECS,
        SETTLE_MAX_WAIT_SECS, SHORT_MAX,
    };
    use std::collections::{HashMap, HashSet};

    /// 2026-10-31: 工作日判定 —— 周末休、周末补班照上、工作日放假照休。
    #[test]
    fn workday_skips_weekends_but_honors_makeup_and_holidays() {
        // 2026-10-31 = 周六, 2026-11-01 = 周日, 2026-11-02 = 周一
        let sat = chrono::NaiveDate::from_ymd_opt(2026, 10, 31).unwrap();
        let sun = chrono::NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let mon = chrono::NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
        let weekdays = vec![1, 2, 3, 4, 5];
        assert!(!schedule_day_active(sat, true, &weekdays, &[], &[]));
        assert!(!schedule_day_active(sun, true, &weekdays, &[], &[]));
        assert!(schedule_day_active(mon, true, &weekdays, &[], &[]));

        // 补班日(周六上班): 命中 extra_work_dates -> 照上
        let makeup = vec!["2026-10-31".to_string()];
        assert!(schedule_day_active(sat, true, &weekdays, &makeup, &[]));
        // 补班日不该顺带把周日也放行
        assert!(!schedule_day_active(sun, true, &weekdays, &makeup, &[]));

        // 法定节假日(周一放假): holidays 优先级最高, 即使当天在 workdays 里也休
        let holidays = vec!["2026-11-02".to_string()];
        assert!(!schedule_day_active(mon, true, &weekdays, &[], &holidays));
        // 同一天同时出现在两张表里时, 节假日优先(宁可少播, 不可乱播)
        let both = vec!["2026-11-02".to_string()];
        assert!(!schedule_day_active(mon, true, &weekdays, &both, &holidays));

        // 作息闸关闭 -> 恒播(七天全上), 与时间闸 enabled=false 的语义一致
        assert!(schedule_day_active(sat, false, &weekdays, &[], &[]));

        // 七天全上的配置: 周末也播(给全年无休的网点留的出口)
        let all_days = vec![1, 2, 3, 4, 5, 6, 7];
        assert!(schedule_day_active(sat, true, &all_days, &[], &[]));

        // 日期串带空白也应命中(配置文件手改容易带空格)
        let padded = vec![" 2026-10-31 ".to_string()];
        assert!(schedule_day_active(sat, true, &weekdays, &padded, &[]));
    }

    /// 老配置(没有 workdays 字段)必须自动落到"周一到周五", 否则升级后周末照旧空扫。
    #[test]
    fn schedule_defaults_to_weekdays_when_field_absent() {
        let json = r#"{"enabled":true,"work_start":"08:00","work_end":"17:00"}"#;
        let s: crate::cfg::Schedule = serde_json::from_str(json).unwrap();
        assert_eq!(s.workdays, vec![1, 2, 3, 4, 5]);
        assert!(s.extra_work_dates.is_empty());
        assert!(s.holidays.is_empty());
    }

    #[test]
    fn number_pool_rejects_impossible_four_digit_misreads() {
        assert!(number_pool_accepts("GA001", 400));
        assert!(number_pool_accepts("GA400", 400));
        assert!(!number_pool_accepts("GA401", 400));
        assert!(!number_pool_accepts("GA1142", 400));
        assert!(!number_pool_accepts("GA2141", 400));
    }

    #[test]
    fn number_pool_filter_keeps_valid_and_drops_outliers() {
        let input = HashSet::from([
            ("GA400".to_string(), "C04".to_string()),
            ("GA401".to_string(), "C04".to_string()),
            ("GA1142".to_string(), "C06".to_string()),
        ]);
        let (kept, rejected) = filter_number_pool(input, 400);
        assert_eq!(
            kept,
            HashSet::from([("GA400".to_string(), "C04".to_string())])
        );
        assert_eq!(rejected, vec!["GA1142".to_string(), "GA401".to_string()]);
    }

    // ———— 时间可信度守卫: RTC 掉电时时钟跳出厂值, 作息闸必须自动失效 ————
    #[test]
    fn time_plausibility_bounds() {
        // 可信: 部署年份起, 到 2035
        assert!(time_plausible_y(2026));
        assert!(time_plausible_y(2027));
        assert!(time_plausible_y(2035));
        // 明显不对: 出厂值/回退值(纽扣电池没电的典型形态)
        assert!(!time_plausible_y(2019));
        assert!(!time_plausible_y(2000));
        assert!(!time_plausible_y(2015));
        // 边界: 2025(部署前, 视为时钟回退) 与 2036(超界)
        assert!(!time_plausible_y(2025));
        assert!(!time_plausible_y(2036));
    }

    #[test]
    fn row_scan_never_stops_before_highlight_row() {
        // 第 1 行是已播旧号，但高亮行在第 2 行：不能提前退出。
        assert!(!should_stop_before_row(1, Some(1), 1, true));
        // 第 1 行读空：空集合不能触发 all() 的真空真。
        assert!(!should_stop_before_row(1, Some(1), 0, false));
        // 已扫过高亮行且所见全是已播号：允许退出。
        assert!(should_stop_before_row(2, Some(1), 1, true));
        // 高亮行未知(夜间/反光/自检页)必须继续扫：这正是"第 1 行旧号、第 2 行新号"
        // 最容易出现的场景，旧逻辑在这里 break 就把漏播留在了兜底路径上。
        assert!(!should_stop_before_row(1, None, 1, true));
        assert!(!should_stop_before_row(2, None, 1, true));
        // 已读到未播号：继续扫后续行补漏。
        assert!(!should_stop_before_row(2, Some(1), 1, false));
    }

    #[test]
    fn configured_highlight_row_overrides_auto_detection() {
        assert_eq!(resolve_highlight_row(Some(2), Some(0), 3), Some(1));
        assert_eq!(resolve_highlight_row(None, Some(1), 3), Some(1));
        assert_eq!(resolve_highlight_row(Some(0), Some(1), 3), Some(1));
        assert_eq!(resolve_highlight_row(Some(4), Some(1), 3), Some(1));
    }

    #[test]
    fn ocr_candidate_quality_prefers_complete_window() {
        let mut c = HashSet::new();
        assert_eq!(ocr_candidate_quality(&c), 0);
        c.insert(("GA200".to_string(), String::new()));
        assert_eq!(ocr_candidate_quality(&c), 1);
        c.insert(("GA200".to_string(), "C08".to_string()));
        assert_eq!(ocr_candidate_quality(&c), 2);
    }

    #[test]
    fn enhancement_pixel_budget_blocks_large_roi() {
        assert!(enhancement_pixels_allowed(570, 69));
        assert!(enhancement_pixels_allowed(1280, 200));
        assert!(!enhancement_pixels_allowed(1280, 1080));
        assert!(!enhancement_pixels_allowed(1600, 80));
    }

    // ———— 跳幅分档: 叫号员会过号, 卡 +2 会漏播; 分档后大跳只多等确认, 不丢号 ————
    #[test]
    fn jump_tiers_by_extra_confirm() {
        // +1~+3: 立即播(与旧行为一致)
        assert_eq!(jump_extra_confirm(1, 10), 0);
        assert_eq!(jump_extra_confirm(3, 10), 0);
        // +4~+max_jump: 多要 2 个确认量
        assert_eq!(jump_extra_confirm(4, 10), 2);
        assert_eq!(jump_extra_confirm(10, 10), 2);
        // 超过 max_jump 但仍 <= 硬上限: 多要 4 个
        assert_eq!(jump_extra_confirm(11, 10), 4);
        assert_eq!(jump_extra_confirm(30, 10), 4);
        // max_jump=0 表示闸门关闭: 全都不要额外确认
        assert_eq!(jump_extra_confirm(40, 0), 0);
        // 上限被调得比"立即档"还小时, 立即档跟着收缩, 不越界放行
        assert_eq!(jump_extra_confirm(1, 1), 0);
        assert_eq!(jump_extra_confirm(2, 1), 4);
    }

    // ———— 确认窗口有效 TTL: 空转退避 vs 确认计数的死锁防线(2026-10-22 现场) ————

    #[test]
    fn ttl_covers_slow_ocr() {
        // OCR 一步 2s: TTL 至少 3 倍耗时, 否则慢机器上一轮都认不完就被清
        assert!(effective_ttl(4.0, 2.0, 3.0, 3.0) >= 6.0);
    }

    #[test]
    fn ttl_survives_backoff_deadlock() {
        // ★回归现场死锁: 过曝帧 0.1s 空输出 -> 兜底间隔被退避拉到 30s,
        //   若 TTL 仍按 3×0.1s 算出 4s, 下一轮开头 retain 就把计数清掉 -> 永不播报。
        let ttl = effective_ttl(4.0, 0.1, 24.0, 30.0);
        assert!(
            ttl > 30.0,
            "TTL 必须大于等待间隔(30s), 否则候选每轮被清零: {ttl}"
        );
    }

    #[test]
    fn ttl_never_shrinks_below_config() {
        // 配置被调大时不能被其他下界压回去
        assert_eq!(effective_ttl(40.0, 0.1, 3.0, 3.0), 40.0);
    }

    #[test]
    fn ttl_normal_case_stays_tight() {
        // 正常情况下不该被过度放大: 帧间隔 3s、OCR 0.5s -> 取 1.5×3=4.5
        assert_eq!(effective_ttl(4.0, 0.5, 3.0, 3.0), 4.5);
    }

    // ———— 行距解析: 绝对像素 vs 相对行高(2026-10-22 换主码流标定事故) ————

    #[test]
    fn pitch_absolute_wins() {
        // 给了绝对像素就以它为准(兼容现有部署)
        assert_eq!(resolve_row_pitch(70, 1.0, 69), 70);
    }

    #[test]
    fn pitch_ratio_follows_resolution() {
        // 相对行高 1.0: 主码流行高 69 -> 69; 换回辅码流行高 37 -> 37, 自动跟随
        assert_eq!(resolve_row_pitch(0, 1.0, 69), 69);
        assert_eq!(resolve_row_pitch(0, 1.0, 37), 37);
        // 略大于 1: 行间有空隙的屏
        assert_eq!(resolve_row_pitch(0, 1.05, 69), 72);
    }

    #[test]
    fn pitch_zero_degrades_to_single_row() {
        // 两者都没给 -> 0 = 不分行(保持旧语义, 不能静默变成多行)
        assert_eq!(resolve_row_pitch(0, 0.0, 69), 0);
    }

    #[test]
    fn pitch_ratio_never_zero_when_asked() {
        // 行高极小也不该算出 0(0 会被当成"不分行")
        assert_eq!(resolve_row_pitch(0, 1.0, 1), 1);
    }

    // ———— 稳帧判定 settle_ready: 现场调优新增逻辑的回归(此前零覆盖) ————

    #[test]
    fn settle_not_ready_during_animation() {
        // 静置窗口未到、变化也未超最长等待 -> 不 ready, 更不该存证
        let (ready, by_change) = settle_ready(10.0, 10.5, false, 10.0);
        assert!(!ready, "换号动画期(静置未到)不该 ready");
        assert!(!by_change);
    }

    #[test]
    fn settle_ready_after_window_marks_by_change() {
        // 静置到点、且这轮变化尚未被认过 -> ready 且标记 by_change(才存定型帧)
        let (ready, by_change) = settle_ready(10.6, 10.5, false, 10.0);
        assert!(ready, "静置到点应 ready");
        assert!(by_change, "未认过的变化应标 by_change");
    }

    #[test]
    fn settle_already_fired_is_ready_but_not_by_change() {
        // 同一段变化已被认过 -> 画面定型(ready), 但不再"为本轮变化"而认
        let (ready, by_change) = settle_ready(10.6, 10.5, true, 10.0);
        assert!(ready);
        assert!(!by_change, "已认过的变化不该重复存证");
    }

    #[test]
    fn settle_overdue_forces_ready_under_persistent_change() {
        // ★关键兜底: 画面一直在变(噪点/曝光漂移)时静置窗口被无限续期,
        //   超过 SETTLE_MAX_WAIT_SECS 必须强制放行, 否则永不 OCR -> 静默不播报。
        let start = 100.0;
        let (ready, by_change) =
            settle_ready(start + SETTLE_MAX_WAIT_SECS, start + 999.0, false, start);
        assert!(ready, "变化持续超最长等待必须强制 ready");
        assert!(by_change, "放行的这一次仍属\"为本轮变化\"而认");
    }

    #[test]
    fn settle_not_overdue_before_max_wait() {
        let start = 100.0;
        let (ready, _) = settle_ready(
            start + SETTLE_MAX_WAIT_SECS - 0.01,
            start + 999.0,
            false,
            start,
        );
        assert!(!ready, "未到最长等待前仍应等画面定型");
    }

    #[test]
    fn settle_zero_change_start_is_never_overdue() {
        // change_start=0.0(启动初值)不该被当成"超长等待"
        let (ready, _) = settle_ready(1e9, 1e9 + 1.0, false, 0.0);
        assert!(!ready, "change_start 未初始化时不应触发最长等待兜底");
    }

    // ———— 按号码票决 number_votes: 修"同一号连播 4 遍"的核心逻辑 ————

    fn conf(items: &[(&str, &str, u32)]) -> HashMap<(String, String), (u32, f64, f64)> {
        items
            .iter()
            .enumerate()
            .map(|(i, (n, w, c))| ((n.to_string(), w.to_string()), (*c, i as f64, i as f64)))
            .collect()
    }

    #[test]
    fn number_votes_merges_windows_into_one_number() {
        // 现场实况: GA174 被窗口误读成 C04/CC04/C045 三个键 -> 必须并成一个号码
        let m = conf(&[
            ("GA174", "C04", 2),
            ("GA174", "CC04", 1),
            ("GA174", "C045", 1),
        ]);
        let v = number_votes(&m);
        assert_eq!(v.len(), 1, "三个窗口候选应并入同一号码");
        assert_eq!(v[0].0, "GA174");
        assert_eq!(v[0].2, 4, "票数应为各窗口之和");
        assert_eq!(v[0].1, "C04", "窗口应取票数最多者");
    }

    #[test]
    fn number_votes_keeps_numbers_separate() {
        let m = conf(&[("GA174", "C04", 3), ("GA175", "C05", 3)]);
        assert_eq!(number_votes(&m).len(), 2, "不同号码不该被合并");
    }

    #[test]
    fn number_votes_tie_takes_latest_window() {
        // 票数并列 -> 取"最近出现"的窗口, 结果与 HashMap 迭代顺序无关
        let mut m: HashMap<(String, String), (u32, f64, f64)> = HashMap::new();
        m.insert(("GA100".into(), "C01".into()), (2, 5.0, 5.0));
        m.insert(("GA100".into(), "C02".into()), (2, 9.0, 9.0));
        let v = number_votes(&m);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].1, "C02", "并列时取最近出现的窗口");
        assert_eq!(v[0].2, 4, "总票数=2+2");
    }

    #[test]
    fn number_votes_reports_earliest_first_seen() {
        // 同号码两个窗口首见时刻不同(3.0 / 7.0) -> [时序] 应取最早的 3.0
        let mut m: HashMap<(String, String), (u32, f64, f64)> = HashMap::new();
        m.insert(("GA200".into(), "C01".into()), (1, 20.0, 3.0));
        m.insert(("GA200".into(), "C02".into()), (1, 25.0, 7.0));
        let v = number_votes(&m);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].3, 3.0, "首见应取最早的一个");
    }

    #[test]
    fn number_votes_empty_is_empty() {
        let m: HashMap<(String, String), (u32, f64, f64)> = HashMap::new();
        assert!(number_votes(&m).is_empty());
    }

    // ———— 空窗口影子候选 drop_shadow_empty ————

    #[test]
    fn drop_shadow_empty_removes_empty_when_windowed_exists() {
        let s: HashSet<(String, String)> = [
            ("GA170".to_string(), "".to_string()),
            ("GA170".to_string(), "C06".to_string()),
        ]
        .into_iter()
        .collect();
        let out = drop_shadow_empty(s);
        assert_eq!(out.len(), 1, "同号码有带窗口版本时应丢掉空窗口那条");
        assert!(out.contains(&("GA170".to_string(), "C06".to_string())));
    }

    #[test]
    fn drop_shadow_empty_keeps_empty_when_no_windowed_sibling() {
        let s: HashSet<(String, String)> = [
            ("GA170".to_string(), "".to_string()),
            ("GA171".to_string(), "".to_string()),
        ]
        .into_iter()
        .collect();
        let out = drop_shadow_empty(s);
        assert_eq!(out.len(), 2, "没有带窗口版本时, 空窗口候选应原样保留");
    }

    // ———— 高亮行"原图补窗口": 号码锁死增强图 (2026-10-28 4 位数误播复盘) ————

    fn mk(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
        pairs
            .iter()
            .map(|(n, w)| (n.to_string(), w.to_string()))
            .collect()
    }

    #[test]
    fn window_fill_borrows_window_when_number_matches() {
        // 增强图只读到号码, 原图读到"号+窗口"且号码一致 -> 借窗口
        let (out, filled, rejected) =
            fill_window_keep_number(&mk(&[("GA013", "")]), &mk(&[("GA013", "C06")]));
        assert_eq!(out, mk(&[("GA013", "C06")]));
        assert_eq!(filled, vec![("GA013".to_string(), "C06".to_string())]);
        assert!(rejected.is_empty());
    }

    #[test]
    fn window_fill_rejects_donor_with_different_number() {
        // ★现场事故回归: 增强图读对 GA013(无窗口), 原图读成 GAO013->GA0013 且带窗口。
        //   旧实现按 q=2 > q=1 整组替换, 播出不存在的 GA0013。现在号码必须一致,
        //   宁可只播号码, 也不采纳原图的错号。
        let (out, filled, rejected) =
            fill_window_keep_number(&mk(&[("GA013", "")]), &mk(&[("GA0013", "C06")]));
        assert_eq!(out, mk(&[("GA013", "")]), "错号不得覆盖已读对的号");
        assert!(filled.is_empty());
        assert_eq!(rejected, vec!["GA0013".to_string()], "被拒的号码要留证据");
    }

    #[test]
    fn window_fill_never_overwrites_existing_window() {
        // 增强图自己有窗口: 原图无论给什么都不改(高亮行原图的窗口本就不可信)
        let (out, filled, _) =
            fill_window_keep_number(&mk(&[("GA170", "C03")]), &mk(&[("GA170", "C06")]));
        assert_eq!(out, mk(&[("GA170", "C03")]));
        assert!(filled.is_empty());
    }

    #[test]
    fn window_fill_noop_when_donor_has_number_only() {
        // 原图也只有号码: 什么都不补, 号码一致不算"拒收"
        let (out, filled, rejected) =
            fill_window_keep_number(&mk(&[("GA170", "")]), &mk(&[("GA170", "")]));
        assert_eq!(out, mk(&[("GA170", "")]));
        assert!(filled.is_empty());
        assert!(rejected.is_empty());
    }

    #[test]
    fn window_fill_handles_empty_and_multi() {
        // 增强图空 -> 不凭空造候选, 原图的号只进"拒收"记录
        let (out, filled, rejected) = fill_window_keep_number(&mk(&[]), &mk(&[("GA001", "C01")]));
        assert!(out.is_empty() && filled.is_empty());
        assert_eq!(rejected, vec!["GA001".to_string()]);

        // 多号码: 缺窗口的借, 已有窗口的不动
        let (out, filled, rejected) = fill_window_keep_number(
            &mk(&[("GA001", ""), ("GA002", "C02")]),
            &mk(&[("GA001", "C05"), ("GA002", "C09")]),
        );
        assert_eq!(out, mk(&[("GA001", "C05"), ("GA002", "C02")]));
        assert_eq!(filled, vec![("GA001".to_string(), "C05".to_string())]);
        assert!(rejected.is_empty());
    }

    #[test]
    fn short_truncates_and_flattens_newlines() {
        assert_eq!(short("  a\r\nb  "), "a b");
        let long = "x".repeat(SHORT_MAX + 5);
        let s = short(&long);
        assert_eq!(s.chars().count(), SHORT_MAX + 1, "超长要截断并加省略号");
        assert!(s.ends_with('…'));
    }

    // ———— 闸门跨日重置: 7×24 连跑时, 次日从头递增的低号码不能被昨天的高基线吞掉 ————

    #[test]
    fn day_rollover_fires_once_per_day_change() {
        let mut day = String::from("2026-10-22");
        // 同日反复维护: 不重置
        assert!(!gate_day_rollover(&mut day, Some("2026-10-22".into())));
        assert!(!gate_day_rollover(&mut day, Some("2026-10-22".into())));
        // 跨到次日: 重置一次, 且只重置一次
        assert!(gate_day_rollover(&mut day, Some("2026-10-23".into())));
        assert!(!gate_day_rollover(&mut day, Some("2026-10-23".into())));
        assert_eq!(day, "2026-10-23");
    }

    #[test]
    fn day_rollover_ignores_missing_local_date() {
        // 取不到本地日(时钟/时区异常): 不重置, 也不丢掉已知的"上次维护日"
        let mut day = String::from("2026-10-22");
        assert!(!gate_day_rollover(&mut day, None));
        assert_eq!(day, "2026-10-22");
    }

    #[test]
    fn day_rollover_first_observation_is_not_a_reset() {
        // 启动时取日失败(maint_day 为空): 第一次读到日期只记录、不当跨日,
        // 免得把刚 seed 出来的基线误清; 之后正常跨日仍要重置。
        let mut day = String::new();
        assert!(!gate_day_rollover(&mut day, Some("2026-10-23".into())));
        assert_eq!(day, "2026-10-23");
        assert!(gate_day_rollover(&mut day, Some("2026-10-24".into())));
    }

    // ———— 跨日重置后"首个无基线候选只标记不播报"(2026-10-23 第八轮中危) ————
    #[test]
    fn seed_only_only_when_marked_and_no_baseline() {
        // 标记未置: 正常播报(不拦)
        assert!(!seed_only_decide(false, false));
        assert!(!seed_only_decide(false, true));
        // 标记生效 + 无基线: 只标记不播报(屏上昨日残留号的处理)
        assert!(seed_only_decide(true, false));
        // 标记生效但前缀已有基线(首日真号已 note_announced): 不吞后续真号
        assert!(!seed_only_decide(true, true));
    }

    // ———— 跨日只标记仅在非营业时段启动启用(2026-10-29 每日 GA001 漏播修复) ————
    #[test]
    fn seed_only_on_startup_off_hours_only() {
        // 营业时段内启动(08:00 定时开机): 不启用 —— 屏上是今日新号而非昨日残留,
        // 启用就会把今日首个真号当残留吞掉并写进去重表(当天永久哑)
        assert!(!seed_only_on_startup(true, Some("08:00"), "08:00", "17:00", true));
        assert!(!seed_only_on_startup(true, Some("12:30"), "08:00", "17:00", true));
        assert!(!seed_only_on_startup(true, Some("16:59"), "08:00", "17:00", true));
        // 非营业时段启动(午夜/下班后/凌晨): 启用 —— 屏上可能是昨日(上一场)尾号,
        // 播出去会毒化基线, 让当天后续小号全天静默
        assert!(seed_only_on_startup(true, Some("17:00"), "08:00", "17:00", true));
        assert!(seed_only_on_startup(true, Some("23:30"), "08:00", "17:00", true));
        assert!(seed_only_on_startup(true, Some("02:00"), "08:00", "17:00", true));
        // 当日已有基线: 无论何时都不启用(重启时屏上已播号由去重表挡住)
        assert!(!seed_only_on_startup(false, Some("02:00"), "08:00", "17:00", true));
        assert!(!seed_only_on_startup(false, Some("12:30"), "08:00", "17:00", true));
        // 作息闸关闭 或 系统时间不可信: 无法判时段 -> 保守启用
        assert!(seed_only_on_startup(true, Some("12:30"), "08:00", "17:00", false));
        assert!(seed_only_on_startup(true, None, "08:00", "17:00", true));
        // 跨零点夜班窗口(22:00~06:00): 04:00 属工作时段 -> 不启用
        assert!(!seed_only_on_startup(true, Some("04:00"), "22:00", "06:00", true));
        assert!(seed_only_on_startup(true, Some("12:00"), "22:00", "06:00", true));
    }

    // ———— 同批播报数值序(2026-10-23 第八轮低危): GA9 必须先于 GA10 ————
    #[test]
    fn announce_sort_key_numeric_not_lexical() {
        let mut v = [
            ("GA10".to_string(), "C01".to_string()),
            ("GA9".to_string(), "C02".to_string()),
            ("GA100".to_string(), "C01".to_string()),
        ];
        v.sort_by_key(|a| announce_sort_key(&a.0, &a.1));
        let order: Vec<&str> = v.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(order, vec!["GA9", "GA10", "GA100"], "数值序: 小号先播");
        // 同号不同窗口: 窗口号字符串序兜底(稳定即可)
        assert!(announce_sort_key("GA9", "C01") < announce_sort_key("GA9", "C02"));
        // 拆不出数字段的候选退回原串排序, 不 panic
        let _ = announce_sort_key("NODIGIT", "C01");
    }

    #[test]
    fn work_window_normal_span() {
        // 普通窗口 08:00~18:00: 含 start、不含 end(18:00 整点交由 stop 侧处理)
        assert!(in_work_window("08:00", "08:00", "18:00"));
        assert!(in_work_window("12:00", "08:00", "18:00"));
        assert!(in_work_window("17:59", "08:00", "18:00"));
        assert!(!in_work_window("18:00", "08:00", "18:00"));
        assert!(!in_work_window("23:30", "08:00", "18:00"));
        assert!(!in_work_window("07:59", "08:00", "18:00"));
    }

    #[test]
    fn work_window_overnight_span() {
        // 跨零点窗口 22:00~06:00(夜班): 22:00 入, 06:00 出, 深夜在窗
        assert!(in_work_window("22:00", "22:00", "06:00"));
        assert!(in_work_window("23:59", "22:00", "06:00"));
        assert!(in_work_window("00:00", "22:00", "06:00"));
        assert!(in_work_window("05:59", "22:00", "06:00"));
        assert!(!in_work_window("06:00", "22:00", "06:00"));
        assert!(!in_work_window("12:00", "22:00", "06:00"));
    }

    #[test]
    fn work_window_edge_cases() {
        // start==end 视为全天; 解析失败宁可静音(false), 不可带着坏配置乱播
        assert!(in_work_window("03:00", "08:00", "08:00"));
        assert!(in_work_window("23:00", "08:00", "08:00"));
        assert!(!in_work_window("12:00", "8点", "18:00"));
        assert!(!in_work_window("12:00", "08:00", ""));
        assert!(!in_work_window("", "08:00", "18:00"));
    }

    // ———— 开机防毒化(2026-10-26 上午 GA219 事故) ————
    // 每天 08:00 全新启动、当天无 OK 记录 -> 基线为空 -> "无基线取最大"会把屏上
    // 残留大号播掉并毒化基线, 之后当天小号全被"号小了差>5"挡死。修法=此时置
    // day_reseed, 让首批候选只标记不播(复用零点跨日的既有机制)。
    #[test]
    fn boot_without_baseline_suppresses_first_batch() {
        // 无基线 + 标记生效: 首个大号(GA219)只标记不播 -> 基线不被毒化
        assert!(
            seed_only_decide(true, false),
            "开机无基线时, 首个候选必须只标记不播, 否则基线被残留大号毒化"
        );
        // 已有基线(当天真号先到并播种): 正常播, 不被标记误吞
        assert!(!seed_only_decide(true, true));
        // 无标记(当天有记录的正常重启): 一切照旧
        assert!(!seed_only_decide(false, false));
        assert!(!seed_only_decide(false, true));
    }

    // ———— 序列重置(2026-10-26 下午 GA174→GA193 事故) ————
    // 判定口径: "远低于基线"= 有基线、v<last、且差值 > 补播窗口 BACKFILL_MAX。
    #[test]
    fn seq_reset_only_for_far_below_baseline() {
        // 基线 193(被误读毒化), 真号 175: 差 18 > 5 -> 够格做序列重置
        assert!(seq_reset_eligible(true, 193, 175));
        // 紧贴基线下方的号属于补播窗口, 走 BACKFILL_MAX 那条路, 不归序列重置管
        assert!(!seq_reset_eligible(true, 175, 173));
        assert!(!seq_reset_eligible(true, 175, 171)); // 差恰好 4, 仍在补播窗口(≤5)内
        assert!(!seq_reset_eligible(true, 175, 170)); // 差恰好 5 = 补播窗口边界, 不算"远低"
        assert!(seq_reset_eligible(true, 175, 169)); // 差 6 > 5 -> 够格
                                                     // 等于/大于基线: 不是"小号", 交给跳幅闸门
        assert!(!seq_reset_eligible(true, 175, 175));
        assert!(!seq_reset_eligible(true, 175, 176));
        // 无基线: 由"无基线取最大"处理, 与序列重置无关
        assert!(!seq_reset_eligible(false, 175, 100));
    }

    // 回归现场: 174 之后误读 193(差 19, 落在 +10~+30 多确认档, 残影稳定驻留骗过
    // 多确认票) —— 说明"多要几票"挡不住稳定残影, 必须靠时间维度的序列重置兜底。
    #[test]
    fn seq_reset_covers_typo_jump_inside_hard_limit() {
        let (last, misread) = (174u32, 193u32);
        assert!(
            misread > last && misread <= last + JUMP_HARD_MAX,
            "误读的 193 当初确实在放行区间内"
        );
        assert!(
            jump_extra_confirm(misread - last, JUMP_HARD_MAX) > 0,
            "当初也只要求了额外确认票"
        );
        // 误读发生后基线变 193, 真号 175 属于"远低于基线" -> 序列重置是唯一出口
        assert!(seq_reset_eligible(true, 193, 175));
    }

    // ———— 改动二: 首轮读空不得清 day_reseed ————
    // 开机第一轮 OCR 常读空(闪黑/未聚焦/换号动画), 若照样清标记, 下一轮屏上残留
    // 的昨日大号就会被"无基线取最大"播出去并把基线毒化成它(2026-10-26 上午 GA219)。
    #[test]
    fn day_reseed_kept_when_round_handled_nothing() {
        // 本轮没有任何候选被处理(读空 / 全被票数或去重挡掉) -> 保住标记
        assert!(keep_day_reseed(false), "读空的一轮不能把 day_reseed 清掉");
        // 本轮真处理过至少一个候选(标记或播报) -> 标记用完, 清掉
        assert!(
            !keep_day_reseed(true),
            "处理过候选后就该清标记, 否则真小号会被当成残留吞掉"
        );
    }

    // 端到端口径: 标记 + 无基线 = 只标记不播(压制开机首批); 清标记后才正常播。
    #[test]
    fn boot_first_batch_marked_then_normal_after_clear() {
        // 首个候选(屏上昨日残留 GA219)到位: 标记生效 + 前缀无基线 -> 只标记
        assert!(seed_only_decide(true, false), "开机首批必须只标记不播");
        // 同一轮真处理过候选 -> 标记用完被清 -> 之后真号(GA002)无基线, 走正常播报
        let marked_after_round = keep_day_reseed(true);
        assert!(!marked_after_round, "处理过候选的轮次结束后标记应已被清");
        assert!(
            !seed_only_decide(marked_after_round, false),
            "标记清掉后真号必须正常播"
        );
        // 若那一轮其实读空(标记没被清), 后续残留号仍然只标记 —— 不毒化基线
        let marked_after_empty = keep_day_reseed(false);
        assert!(marked_after_empty, "读空的轮次结束后标记必须留着");
        assert!(
            seed_only_decide(marked_after_empty, false),
            "读空后残留大号仍须只标记"
        );
    }

    // ———— 改动三: 序列重置(静帧内连续稳定 N 秒) ————
    /// 模拟"同一静帧段内每轮都看见同一个号", 返回首次够钟时的轮次号(1-based)。
    fn rounds_until_fire() -> u32 {
        // 现场主码流约 1.3s 一轮, 8s 阈值 -> 约 7 轮
        let tick = 1.3f64;
        let mut since: Option<f64> = None;
        for i in 1..=60u32 {
            let now = i as f64 * tick;
            let (s, fire) = seq_reset_tick(since, now);
            since = Some(s);
            if fire {
                return i;
            }
        }
        panic!("60 轮内应触发序列重置");
    }

    #[test]
    fn seq_reset_fires_after_secs_within_one_static_segment() {
        // 基线 193(被 GA193 误读毒化), 真号 175 稳定出现 -> 约 8s 后重置并放行
        let at = rounds_until_fire();
        assert!(
            (6..=8).contains(&at),
            "1.3s/轮下应在第 6~8 轮(≈8s)触发, 实际第 {} 轮",
            at
        );
        // 触发条件与"远低于基线"口径一致: 193 vs 175 差 18 够格
        assert!(seq_reset_eligible(true, 193, 175));
    }

    #[test]
    fn seq_reset_not_fired_before_secs() {
        // 第一轮只起算, 不放行; 未满 SEQ_RESET_SECS 一律挡
        let (s0, f0) = seq_reset_tick(None, 100.0);
        assert_eq!(s0, 100.0, "首次看见只记起点(取当前墙钟)");
        assert!(!f0, "起算当轮绝不放行");

        let (_, f) = seq_reset_tick(Some(100.0), 100.0 + SEQ_RESET_SECS - 0.1);
        assert!(
            !f,
            "差一点满 {}s 也不能放行 —— 残影就活在这个区间",
            SEQ_RESET_SECS
        );

        let (_, f) = seq_reset_tick(Some(100.0), 100.0 + SEQ_RESET_SECS);
        assert!(f, "刚好满 {}s 应当放行", SEQ_RESET_SECS);

        // 静帧段换了(调用方清表)后重新起算, 依旧不会提前触发
        let (s, fire) = seq_reset_tick(None, 200.0);
        assert_eq!((s, fire), (200.0, false), "换段后从头计时");
    }
}
