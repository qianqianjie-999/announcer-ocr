// announce-rs — 叫号屏摄像头 OCR 语音播报(Rust 版)
// 链路: 摄像头(ffmpeg管道/simulate) -> ROI 帧差变化检测 -> 触发OCR(tesseract)
//       -> 正则校验 -> 多轮观察确认(非阻塞状态机) -> 去重 -> FIFO队列
//       -> 本地TTS播报 -> 日志留痕。
// 与 Python 版(announce.py)逐行为对齐复刻, 差异仅有:
//   - OCR 用 tesseract 子进程(靶机实测 1.43s/次全对), 不再内置 onnxruntime;
//   - 取流用 ffmpeg 子进程管道(纯 Rust 依赖, 可静态链接);
//   - 兜底第二路放大后不做 CLAHE(纯 Rust 无 OpenCV), 仅 2x 放大重试。
//
// 本文件结构(二轮重构, 原 run() 单函数 428 行 -> Loop 结构体 + 分阶段方法):
//   run()        启动期一次性动作: 读配置 / 抢单实例锁 / 装信号 / 清 _live
//   Loop::build  把不变参数与跨轮状态收进结构体
//   Loop::run    主循环骨架: 停止检查 -> tick -> 按 fps 补睡眠 -> 收尾
//   Loop::tick   一轮: 取帧 -> 帧差/静帧 -> 诊断 -> 快速确认 -> (按需)OCR -> 维护 -> 健康心跳
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
    /// 4096 条硬上限兜底: 即使 announced_ttl_sec 被配成"永不清理"也不会吃内存。
    announced: AgedMap<(String, String), ()>,
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
        let mut hit: Vec<(String, String)> = Vec::new();
        for (c, v) in self.confirm.iter() {
            // c: &(号码, 窗口), v: (累计轮数, 最后出现时刻, 首次出现时刻)
            // last_seen >= static_since: 候选必须 sighting 于"当前静帧段"内,
            // 旧静帧段残留的假影候选永远进不了快速确认(走原投票路径)。
            if !self.announced.contains_key(c) && v.1 >= self.static_since {
                hit.push(c.clone());
            }
        }
        if hit.is_empty() {
            return;
        }
        hit.sort();
        let t_ann = now_secs();
        for cand in hit {
            let first = match self.confirm.get(&cand) {
                Some(&(_, _, f)) => f,
                None => t_ann,
            };
            self.announced.insert(cand.clone(), (), t_ann);
            let raw = self.cand_raw.get(&cand).cloned().unwrap_or_default();
            logger::log(&self.cfg.log_file, &cand.0, &cand.1, &raw, "OK");
            self.ann.speak(&cand.0, &cand.1); // 入FIFO队列, 播报线程按顺序播
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
    /// 触发条件, 三条路径并联:
    ///   ① 帧差判定有变化 -> 快速路径(省 CPU 的主力)
    ///   ② 处于观察期(有候选待确认) -> 完成多轮确认
    ///   ③ 距上次 OCR 超过兜底间隔 -> 保险丝(防 ROI 略大时帧差漏检)
    ///     间隔自适应: 屏面静止且上一轮读出过内容(都知道是什么、且都已播报)时,
    ///     重读同一批像素纯属空转, 放宽到 static_fb_sec(默认10s);
    ///     内容未知(INVALID/启动)或有变化时维持 fb_eff 节奏(含空转退避)不变。
    fn maybe_ocr(&mut self, frame: &Arc<capture::Frame>, changed: bool, now: f64) {
        let gap = now - self.last_ocr_time;
        let due_change = (changed || now < self.active_until) && gap >= self.det.min_interval_sec;
        let ins_gap = if changed || !self.last_read_ok {
            self.fb_eff
        } else {
            self.static_fb_sec.max(self.fb_eff)
        };
        let due_fallback = self.fb_sec > 0.0 && gap >= ins_gap;
        if !(due_change || due_fallback) {
            return;
        }
        self.last_ocr_time = now;
        let iter_t0 = now_secs();
        let crop = capture::crop_rgb(frame, self.roi);

        // 触发存证: 因"画面变化"跑 OCR 时把所看画面存图,
        // 排查「心跳有变化但读不出」直接看图。
        if self.save_frames && changed && now - self.last_save >= 2.0 && self.n_saved < 20 {
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

        // 先取出结果再分支, 让 recognize 对 self.ocr 的借用立即结束。
        let recognized = self.ocr.recognize(&crop);
        match recognized {
            Ok(raw) => {
                let mut raw = raw;
                let mut cands = self.ocr.parse(&raw);
                if cands.is_empty() && self.roi.2 <= 1280 {
                    // 兜底增强识别链: 原图(上面已跑) -> 增强图 psm11 -> 增强图 psm6。
                    // ★为什么要增强: 真实摄像头画面是"白字深底+小字+糊", tesseract 对
                    //   原图几乎全乱码(靶机实测 [播报] 长期为 0 的根因之一), 沙盒用干净
                    //   测试卡测不出来。2x放大+红通道+自动对比度+反相后靶机实测能把
                    //   "GA777到C03号窗口/GA999到C01号窗口"读出来。psm11 与 psm6 错误
                    //   模式不同(实测同一图一个读对窗口、一个读对号码), 各试一次先中先用。
                    let up = detect::enhance_for_ocr(&crop);
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
                // 本轮 OCR 总耗时(含可能的 2x 兜底) —— 主循环被它阻塞的时长
                let iter_secs = now_secs() - iter_t0;
                self.diag_ocr_n += 1;
                self.diag_ocr_total += iter_secs;
                if iter_secs > self.diag_ocr_max {
                    self.diag_ocr_max = iter_secs;
                }
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
                    self.cand_raw.insert(cand.clone(), raw.clone(), now);
                    if !self.announced.contains_key(cand) && self.prefetched.insert(cand.clone()) {
                        if self.prefetched.len() > 64 {
                            self.prefetched.clear();
                        }
                        self.ann.prefetch(&cand.0, &cand.1);
                    }
                }
                // 只对"尚未播报"的候选续观察期。若对已播报的号码也续期,
                // 屏幕一直显示号码时观察期会被无限续期 -> OCR 变相常开白烧 CPU。
                if cands.iter().any(|c| !self.announced.contains_key(c)) {
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
                    if cnt >= self.need_stable && !self.announced.contains_key(&cand) {
                        self.announced.insert(cand.clone(), (), now);
                        logger::log(&self.cfg.log_file, &cand.0, &cand.1, &raw, "OK");
                        self.ann.speak(&cand.0, &cand.1); // 入FIFO队列, 播报线程按顺序播
                        self.last_announce = now; // 健康心跳: 真有号播了, 复位闲置计时
                        println!(
                            "[时序] 首见→开始播报 {:.0} ms (确认{}轮, 本轮OCR {:.1}s); 另加相机/网络/轮询约 0.3~0.8 s",
                            (now - first) * 1000.0,
                            cnt,
                            iter_secs
                        );
                    }
                }
                if cands.is_empty() && changed {
                    logger::log(&self.cfg.log_file, "", "", &raw, "INVALID");
                }
            }
            Err(e) => {
                let iter_secs = now_secs() - iter_t0;
                self.diag_ocr_n += 1;
                self.diag_ocr_total += iter_secs;
                if iter_secs > self.diag_ocr_max {
                    self.diag_ocr_max = iter_secs;
                }
                // 超时/失败同样计入空转退避, 别让病态帧把兜底间隔钉在基础值上
                self.empty_streak = self.empty_streak.saturating_add(1);
                let k = 1u32 << self.empty_streak.min(4);
                self.fb_eff = (self.fb_sec * k as f64).min(30.0);
                self.last_read_ok = false; // 这轮没读出内容, 屏上是什么是未知的
                println!("[OCR失败] {}", e);
            }
        }
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
