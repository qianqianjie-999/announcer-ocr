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
    let overdue =
        !change_fired && change_start > 0.0 && now - change_start >= SETTLE_MAX_WAIT_SECS;
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
    /// 上次"成功入队播报"的时刻(启动时即置为当前, 避免刚起就误报) —— 健康心跳用。
    last_announce: f64,
    /// 上次打"健康告警"的时刻。用于把告警限流成"每阈值周期最多一条", 防止刷屏。
    last_health_warn: f64,
}

impl Loop {
    /// 启动期一次性构造: 解析 ROI、建 OCR / 播报器 / 取流通道, 初始化全部状态。
    fn build(cfg: Config) -> Result<Loop, String> {
        let roi_v = cfg
            .roi
            .clone()
            .filter(|r| r.len() == 4)
            .ok_or("请先运行 calibrate.py 框选识别区域")?;
        let roi = (roi_v[0] as usize, roi_v[1] as usize, roi_v[2] as usize, roi_v[3] as usize);

        let ocr = ocr::Ocr::new(&cfg.ocr)?;
        let ann = announcer::Announcer::new(&cfg)?;
        let src = capture::open(&cfg.source(), &cfg)?;

        let det = cfg.detect.clone();
        let fb_sec = det.fallback_ocr_sec;
        let now = now_secs();
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
            announced: AgedMap::new(Expiry::from_ttl_sec(cfg.announced_ttl_sec), 4096),
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

        let prepared = detect::prepare(&frame, self.roi);
        // ROI 落在帧外(源尺寸变化/配置不匹配): 跳帧防 panic, 只告警一次不刷屏
        if prepared.gray.width() == 0 {
            if !self.roi_warned {
                self.roi_warned = true;
                eprintln!(
                    "[警告] ROI {:?} 超出帧 {}x{}, 跳帧等待恢复",
                    self.roi, frame.w, frame.h
                );
            }
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
        let tag = if self.diag_max > self.det.changed_ratio { ">>> 有变化" } else { "静" };
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
        println!(
            "[监测] 近{:.0}s 最大帧差={:.4} 阈值={:.4} 帧数={} {}{}",
            span, self.diag_max, self.det.changed_ratio, self.diag_frames, tag, ocr_info
        );
        self.diag_last = now;
        self.diag_max = 0.0;
        self.diag_frames = 0;
        self.diag_ocr_n = 0;
        self.diag_ocr_total = 0.0;
        self.diag_ocr_max = 0.0;
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
        if self.confirm_ticks < 1
            || self.static_streak < self.confirm_ticks
            || self.confirm.is_empty()
        {
            return;
        }
        // 与正式播报路径同口径: 按【号码】聚合, 窗口取票数最多的那条。
        let mut hit: Vec<(String, String, u32, f64, f64)> = number_votes(&self.confirm)
            .into_iter()
            // last_seen >= static_since: 候选必须 sighting 于"当前静帧段"内,
            // 旧静帧段残留的假影候选永远进不了快速确认(走原投票路径)。
            .filter(|(n, _, _, _, last)| {
                !self.announced.contains_key(n) && *last >= self.static_since
            })
            .collect();
        if hit.is_empty() {
            return;
        }
        // 元组含 f64(无 Ord), 按"号码+窗口"排序保证播报顺序稳定。
        hit.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        let t_ann = now_secs();
        for (number, window, _votes, first, _last) in hit {
            self.announced.insert(number.clone(), (), t_ann);
            let raw = self
                .cand_raw
                .get(&(number.clone(), window.clone()))
                .cloned()
                .unwrap_or_default();
            logger::log(&self.cfg.log_file, &number, &window, &raw, "OK");
            self.ann.speak(&number, &window); // 入FIFO队列, 播报线程按顺序播
            self.last_announce = t_ann; // 健康心跳: 真有号播了, 复位闲置计时
            println!(
                "[时序] 首见→开始播报 {:.0} ms (静帧确认{}轮, 免第二轮OCR); 另加相机/网络/轮询约 0.3~0.8 s",
                (t_ann - first) * 1000.0,
                self.static_streak
            );
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
        let by_change = match self.ocr_due(changed, now) {
            Some(v) => v,
            None => return,
        };
        self.last_ocr_time = now;
        // 本轮已覆盖这次变化(不论由哪条路径触发), 同一段静止画面不再重复触发。
        self.change_fired = true;
        let iter_t0 = now_secs();
        let crop = capture::crop_rgb(frame, self.roi);
        self.save_evidence(&crop, by_change, now);
        match self.recognize_candidates(&crop) {
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
        self.last_save = now;
        self.n_saved += 1;
        let _ = std::fs::create_dir_all(SAVE_DIR);
        let p = format!("{}/trig_{:02}.jpg", SAVE_DIR, self.n_saved);
        let res = std::fs::File::create(&p).and_then(|f| {
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                std::io::BufWriter::new(f),
                92,
            );
            enc.encode_image(&image::DynamicImage::ImageRgb8(crop.clone()))
                .map_err(std::io::Error::other)
        });
        match res {
            Ok(()) => println!("[存证] OCR所见画面 -> {}", p),
            Err(e) => println!("[存证失败] {}", e),
        }
    }

    /// 对裁剪图跑 OCR 并解析候选; 原图读不出且 ROI 不大时自动走"增强图"兜底链路。
    /// 返回 `(本轮原始文本, 候选集合)`; 只有 tesseract 子进程本身失败才返 Err。
    fn recognize_candidates(
        &self,
        crop: &image::RgbImage,
    ) -> Result<(String, HashSet<(String, String)>), String> {
        let mut raw = self.ocr.recognize(crop)?;
        let mut cands = self.ocr.parse(&raw);
        if cands.is_empty() && self.roi.2 <= 1280 {
            // 兜底增强识别链: 原图(上面已跑) -> 增强图 psm11 -> 增强图 psm6。
            // ★为什么要增强: 真实摄像头画面是"白字深底+小字+糊", tesseract 对
            //   原图几乎全乱码(靶机实测 [播报] 长期为 0 的根因之一), 沙盒用干净
            //   测试卡测不出来。2x放大+红通道+自动对比度+反相后靶机实测能把
            //   "GA777到C03号窗口/GA999到C01号窗口"读出来。psm11 与 psm6 错误
            //   模式不同(实测同一图一个读对窗口、一个读对号码), 各试一次先中先用。
            let up = detect::enhance_for_ocr(crop);
            for psm in [self.ocr.default_psm(), 6u32] {
                if !cands.is_empty() {
                    break;
                }
                if let Ok(raw2) = self.ocr.recognize_psm(&up, psm) {
                    let c2 = self.ocr.parse(&raw2);
                    if !c2.is_empty() {
                        cands = c2;
                        raw = raw2;
                    }
                }
            }
        }
        // 同一号码若另有带窗口的候选, 丢弃它的空窗口版本 —— 否则同一号码会
        // 先后播 "请GA170到号窗口办理"(半截话) 和 "请GA170到C06号窗口办理"。
        // 详见 drop_shadow_empty() 的注释。
        Ok((raw, drop_shadow_empty(cands)))
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
        // ★确认窗口自适应: OCR 一步就要 1.5~10s 时, 固定 4s 的 TTL 会在
        //   下一轮到来前把候选清掉 -> 永远凑不齐 stable_frames 轮 -> 不播报。
        //   (靶机实测就是这样死的: 圈速 24s > TTL 4s。)
        //   有效 TTL = max(配置值, 3×本轮OCR耗时), 静止的号不会被误清。
        let ttl_eff = self.ttl.max(3.0 * iter_secs);
        // 空转退避: 连续无候选就把兜底间隔翻倍(上限 30s), 有候选立刻复位。
        if cands.is_empty() {
            self.empty_streak = self.empty_streak.saturating_add(1);
            let k = 1u32 << self.empty_streak.min(4);
            self.fb_eff = (self.fb_sec * k as f64).min(30.0);
        } else {
            self.empty_streak = 0;
            self.fb_eff = self.fb_sec;
        }
        // TTS 预热: 第一轮看到候选就后台合成(结果落在常驻服务缓存里),
        // 等第 stable_frames 轮确认通过再播时即命中缓存(~毫秒),
        // 相当于把 1.8s 的合成耗时从关键路径上整体藏掉。
        self.last_read_ok = !cands.is_empty();
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
        // 只对"因变化而跑"的那一轮记 INVALID: 兜底轮读到空属正常, 记了会刷屏。
        if cands.is_empty() && by_change {
            logger::log(&self.cfg.log_file, "", "", raw, "INVALID");
        }
    }

    /// 按【号码】票决后播报: 票数达到 need_stable 且该号码未播报过才入队。
    /// 聚合口径见 number_votes() —— 同一号码的多个窗口候选并成一票, 只播一次。
    fn announce_winners(&mut self, raw: &str, now: f64, iter_secs: f64) {
        let mut winners = number_votes(&self.confirm);
        winners.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        for (number, window, votes, first, _last) in winners {
            if votes < self.need_stable || self.announced.contains_key(&number) {
                continue;
            }
            self.announced.insert(number.clone(), (), now);
            logger::log(&self.cfg.log_file, &number, &window, raw, "OK");
            self.ann.speak(&number, &window); // 入FIFO队列, 播报线程按顺序播
            self.last_announce = now; // 健康心跳: 真有号播了, 复位闲置计时
            println!(
                "[时序] 首见→开始播报 {:.0} ms (确认{}轮, 本轮OCR {:.1}s); 另加相机/网络/轮询约 0.3~0.8 s",
                (now - first) * 1000.0,
                votes,
                iter_secs
            );
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
    /// ② 日志写满阈值就归档(旧实现只管写, 写满 SD 卡只能等部署层第二天来收)。
    /// 清理/轮转都打日志: 旧实现的"静默软故障"正是因为它悄悄什么都不做 ——
    /// 让隐式维护显形, 现场看一眼 journal 就知道治理在不在工作。
    fn maintenance(&mut self, now: f64) {
        if now - self.last_maint < self.maint_every {
            return;
        }
        self.last_maint = now;
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
        if let Some(p) =
            logger::rotate_if_needed(&self.cfg.log_file, dir, self.log_max_bytes, self.cfg.log_keep)
        {
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
    use super::{drop_shadow_empty, number_votes, settle_ready, SETTLE_MAX_WAIT_SECS};
    use std::collections::{HashMap, HashSet};

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
        let (ready, _) =
            settle_ready(start + SETTLE_MAX_WAIT_SECS - 0.01, start + 999.0, false, start);
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
        let m = conf(&[("GA174", "C04", 2), ("GA174", "CC04", 1), ("GA174", "C045", 1)]);
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
}
