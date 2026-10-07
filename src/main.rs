// announce-rs — 叫号屏摄像头 OCR 语音播报(Rust 版)
// 链路: 摄像头(ffmpeg管道/simulate) -> ROI 帧差变化检测 -> 触发OCR(tesseract)
//       -> 正则校验 -> 多轮观察确认(非阻塞状态机) -> 去重 -> FIFO队列
//       -> 本地TTS播报 -> 日志留痕。
// 与 Python 版(announce.py)逐行为对齐复刻, 差异仅有:
//   - OCR 用 tesseract 子进程(靶机实测 1.43s/次全对), 不再内置 onnxruntime;
//   - 取流用 ffmpeg 子进程管道(纯 Rust 依赖, 可静态链接);
//   - 兜底第二路放大后不做 CLAHE(纯 Rust 无 OpenCV), 仅 2x 放大重试。
mod announcer;
mod capture;
mod cfg;
mod detect;
mod logger;
mod ocr;
mod tts;

use cfg::{Config, SourceKind};
use image::GrayImage;
use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
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

fn run() -> Result<(), String> {
    let cfg = Config::load("config.json")?;
    // 锁端口可配置: 便于不打扰生产实例的前提下另起模拟实例自测
    let _lock = acquire_lock(cfg.lock_port)?;

    let roi_v = cfg
        .roi
        .clone()
        .filter(|r| r.len() == 4)
        .ok_or("请先运行 calibrate.py 框选识别区域")?;
    let roi = (roi_v[0] as usize, roi_v[1] as usize, roi_v[2] as usize, roi_v[3] as usize);

    let ocr = ocr::Ocr::new(&cfg.ocr)?;
    let ann = announcer::Announcer::new(&cfg);
    let mut src = capture::open(&cfg.source(), &cfg)?;

    let det = cfg.detect.clone();
    let interval = 1.0 / cfg.fps.max(0.1);
    let need_stable = det.stable_frames;
    let ttl = det.confirm_ttl_sec;
    let active_sec = det.active_sec;
    let fb_sec = det.fallback_ocr_sec;

    let mut prev_gray: Option<GrayImage> = None;
    let mut announced: HashSet<(String, String)> = HashSet::new();
    // 非阻塞确认状态机: {候选: (累计轮数, 最后出现时刻, 首次出现时刻)}
    // 关键: 不在确认期间 sleep 阻塞主循环, 否则屏幕连续跳号会被"盲区"吞掉
    let mut confirm: HashMap<(String, String), (u32, f64, f64)> = HashMap::new();
    let mut last_ocr_time = 0.0f64;
    let mut active_until = 0.0f64;
    // 已预热过 TTS 的候选(第一轮识别到就后台合成, 把合成耗时藏进确认期)。
    // 集合只增不删但有上界: 满了整表清空, 语义是"近期不重复预热", 足够。
    let mut prefetched: HashSet<(String, String)> = HashSet::new();
    // 兜底复查的空转退避: 连续无候选时把兜底间隔翻倍, 上限 30s;
    // 一旦有候选(或真实变化)立刻回到基础间隔。防"静置画面每 3s 烧一次 OCR"。
    let mut empty_streak: u32 = 0;
    let mut fb_eff = fb_sec;
    // —— 静帧治理(2026-09-28 延迟专项) ————————————————————————————
    // 靶机 journalctl 实测: 屏面完全静止(帧差 0.0000)、号码都已播报时,
    // 兜底 OCR 仍每 3s 空跑一次(cands 非空把退避复位了), 单次 1.6~4.8s,
    // 把主循环占满 -> 真变化平均要白等 1.45s 才被看见, 且确认还要再跑一轮
    // 完整 OCR。端到端 5~10s 的两大来源就在这里。
    let static_fb_sec = det.static_fb_sec;        // 静止+已知内容时的兜底间隔
    let confirm_ticks = det.static_confirm_ticks; // 静帧快速确认轮数(0=关闭)
    let mut static_streak: u32 = 0;               // 连续"无变化"的轮数
    let mut static_since = 0.0f64;                // 当前静帧段起点(最后一条 changed 帧)
    let mut last_read_ok = false;                 // 上一轮 OCR 是否读出了内容
    // 候选首次识别时的原文(静帧快速确认路径没有当轮 raw, 落日志用)
    let mut cand_raw: HashMap<(String, String), String> = HashMap::new();
    // 诊断窗口内的 OCR 统计(次数/总耗时/最长), 拼进 [监测] 行便于远程排障
    let mut diag_ocr_n: u32 = 0;
    let mut diag_ocr_total = 0.0f64;
    let mut diag_ocr_max = 0.0f64;

    // 诊断心跳: 每 diag_every 秒报一次"近N秒最大帧差"。
    // 排查"举了卡却没反应"时靠它区分【没举卡】和【阈值漏检】。
    let diag_on = cfg.diag_heartbeat;
    let diag_every = cfg.diag_every_sec;
    let mut diag_last = now_secs();
    let mut diag_max = 0.0f64;
    let mut diag_frames: u64 = 0;

    // 触发存证: 因"画面变化"跑 OCR 时把所看画面存图, 排查「心跳有变化但读不出」直接看图。
    let save_frames = cfg.save_trigger_frames;
    let save_dir = "_live";
    let mut last_save = 0.0f64;
    let mut n_saved: u32 = 0;

    let src_desc = match cfg.source() {
        SourceKind::Simulate => "simulate".to_string(),
        SourceKind::Net(u) => u,
        SourceKind::Device(d) => d,
    };
    println!(
        "运行中: 源={}, ROI=({}, {}, {}, {}), 稳定帧数={}, Ctrl+C 退出",
        src_desc, roi.0, roi.1, roi.2, roi.3, det.stable_frames
    );

    loop {
        let loop_start = now_secs();
        let frame = match src.read() {
            Some(f) => f,
            None => {
                println!("[错误] 取帧失败，1秒后重试");
                std::thread::sleep(Duration::from_secs(1));
                prev_gray = None;
                continue;
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

        let mut prepared = detect::prepare(&frame, roi);
        detect::blur5(&mut prepared.gray);

        let mut changed = true;
        let mut ratio: Option<f64> = None;
        if let Some(prev) = &prev_gray {
            // 先抵消"整体亮度漂移"再比: 摄像头自动曝光会持续微调整帧亮度,
            // 原始 absdiff 会把漂移当"画面变化"导致空场景反复触发 OCR。
            if let Some(r) = detect::change_ratio(&prepared.gray, prev, det.diff_threshold) {
                ratio = Some(r);
                changed = r > det.changed_ratio;
            }
        }
        prev_gray = Some(prepared.gray.clone());

        let now = now_secs();

        // —— 静帧计数 ——————————————————————————————————————————
        // changed=true 的那一轮是"静帧段"起点; 之后每连续一轮无变化 streak+1。
        // 快速确认、静态兜底间隔都建立在它上面。
        if changed {
            static_streak = 0;
            static_since = now;
        } else {
            static_streak = static_streak.saturating_add(1);
        }

        // —— 诊断心跳 ————————————————————————————————————————
        if let Some(r) = ratio {
            if r > diag_max {
                diag_max = r;
            }
        }
        diag_frames += 1;
        if diag_on && now - diag_last >= diag_every {
            let span = now - diag_last;
            let tag = if diag_max > det.changed_ratio { ">>> 有变化" } else { "静" };
            let ocr_info = if diag_ocr_n > 0 {
                format!(
                    " OCR={}次 均匀{:.1}s 最长{:.1}s",
                    diag_ocr_n,
                    diag_ocr_total / f64::from(diag_ocr_n),
                    diag_ocr_max
                )
            } else {
                " OCR=0次".to_string()
            };
            println!(
                "[监测] 近{:.0}s 最大帧差={:.4} 阈值={:.4} 帧数={} {}{}",
                span, diag_max, det.changed_ratio, diag_frames, tag, ocr_info
            );
            diag_last = now;
            diag_max = 0.0;
            diag_frames = 0;
            diag_ocr_n = 0;
            diag_ocr_total = 0.0;
            diag_ocr_max = 0.0;
        }

        // —— 静帧快速确认 ————————————————————————————————————
        // 候选出现过一次, 且画面已连续 confirm_ticks 轮无变化 -> 直接确认播报,
        // 免掉第二轮 OCR。原理: tesseract 对相同输入是确定性的, 像素不变复读必然
        // 同文; 第二轮唯一防的是"转瞬即逝的假影", 而静帧观察直接测量了"画面稳定",
        // 防护等价, 却省下一整轮 OCR(靶机实测 1.6~4.8s —— 5~10s 延迟的最大单项)。
        // 防误报关键: 候选必须是在"当前静帧段内" sighting 的(last_seen >= static_since)。
        // 反光假影在晃动期被读到, 晃动停止后 static_since 更新, 假影因 last_seen
        // 落在旧段而永远凑不齐快速确认 -> 只能走原投票路径(和旧行为一致)。
        // 观察期内一旦又出现变化(changed=true), streak 清零, 自动退回两轮投票。
        if confirm_ticks >= 1
            && static_streak >= confirm_ticks
            && !confirm.is_empty()
        {
            let mut hit: Vec<(String, String)> = Vec::new();
            for (c, v) in confirm.iter() {
                // c: &(号码, 窗口), v: (累计轮数, 最后出现时刻, 首次出现时刻)
                // last_seen >= static_since: 候选必须 sighting 于"当前静帧段"内,
                // 旧静帧段残留的假影候选永远进不了快速确认(走原投票路径)。
                if !announced.contains(c) && v.1 >= static_since {
                    hit.push(c.clone());
                }
            }
            if !hit.is_empty() {
                hit.sort();
                let t_ann = now_secs();
                for cand in hit {
                    let first = match confirm.get(&cand) {
                        Some(&(_, _, f)) => f,
                        None => t_ann,
                    };
                    announced.insert(cand.clone());
                    let raw = cand_raw.get(&cand).cloned().unwrap_or_default();
                    logger::log(&cfg.log_file, &cand.0, &cand.1, &raw, "OK");
                    ann.speak(&cand.0, &cand.1); // 入FIFO队列, 播报线程按顺序播
                    println!(
                        "[时序] 首见→开始播报 {:.0} ms (静帧确认{}轮, 免第二轮OCR); 另加相机/网络/轮询约 0.3~0.8 s",
                        (t_ann - first) * 1000.0,
                        static_streak
                    );
                }
                // 已无待确认候选: 撤观察期。否则 active_until 还会让主循环
                // 再空跑一轮"确认 OCR"(2~5s CPU), 白占机器。
                active_until = 0.0;
            }
        }

        // 触发条件, 三条路径并联:
        //   ① 帧差判定有变化 -> 快速路径(省 CPU 的主力)
        //   ② 处于观察期(有候选待确认) -> 完成多轮确认
        //   ③ 距上次 OCR 超过兜底间隔 -> 保险丝(防 ROI 略大时帧差漏检)
        //     间隔自适应: 屏面静止且上一轮读出过内容(都知道是什么、且都已播报)时,
        //     重读同一批像素纯属空转, 放宽到 static_fb_sec(默认10s);
        //     内容未知(INVALID/启动)或有变化时维持 fb_eff 节奏(含空转退避)不变。
        let gap = now - last_ocr_time;
        let due_change = (changed || now < active_until) && gap >= det.min_interval_sec;
        let ins_gap = if changed || !last_read_ok { fb_eff } else { static_fb_sec.max(fb_eff) };
        let due_fallback = fb_sec > 0.0 && gap >= ins_gap;
        if due_change || due_fallback {
            last_ocr_time = now;
            let iter_t0 = now_secs();
            let crop = capture::crop_rgb(&frame, roi);

            if save_frames && changed && now - last_save >= 2.0 && n_saved < 20 {
                last_save = now;
                n_saved += 1;
                let _ = std::fs::create_dir_all(save_dir);
                let p = format!("{}/trig_{:02}.jpg", save_dir, n_saved);
                let res = std::fs::File::create(&p).and_then(|f| {
                    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                        std::io::BufWriter::new(f),
                        92,
                    );
                    enc.encode_image(&image::DynamicImage::ImageRgb8(crop.clone()))
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
                });
                match res {
                    Ok(()) => println!("[存证] OCR所见画面 -> {}", p),
                    Err(e) => println!("[存证失败] {}", e),
                }
            }

            match ocr.recognize(&crop) {
                Ok(raw) => {
                    let mut raw = raw;
                    let mut cands = ocr.parse(&raw);
                    if cands.is_empty() && roi.2 <= 1280 {
                        // 兜底增强识别链: 原图(上面已跑) -> 增强图 psm11 -> 增强图 psm6。
                        // ★为什么要增强: 真实摄像头画面是"白字深底+小字+糊", tesseract 对
                        //   原图几乎全乱码(靶机实测 [播报] 长期为 0 的根因之一), 沙盒用干净
                        //   测试卡测不出来。2x放大+红通道+自动对比度+反相后靶机实测能把
                        //   "GA777到C03号窗口/GA999到C01号窗口"读出来。psm11 与 psm6 错误
                        //   模式不同(实测同一图一个读对窗口、一个读对号码), 各试一次先中先用。
                        let up = detect::enhance_for_ocr(&crop);
                        for psm in [ocr.default_psm(), 6u32] {
                            if cands.len() > 0 {
                                break;
                            }
                            if let Ok(raw2) = ocr.recognize_psm(&up, psm) {
                                let c2 = ocr.parse(&raw2);
                                if !c2.is_empty() {
                                    cands = c2;
                                    raw = raw2;
                                }
                            }
                        }
                    }
                    // 本轮 OCR 总耗时(含可能的 2x 兜底) —— 主循环被它阻塞的时长
                    let iter_secs = now_secs() - iter_t0;
                    diag_ocr_n += 1;
                    diag_ocr_total += iter_secs;
                    if iter_secs > diag_ocr_max {
                        diag_ocr_max = iter_secs;
                    }
                    // ★确认窗口自适应: OCR 一步就要 1.5~10s 时, 固定 4s 的 TTL 会在
                    //   下一轮到来前把候选清掉 -> 永远凑不齐 stable_frames 轮 -> 不播报。
                    //   (靶机实测就是这样死的: 圈速 24s > TTL 4s。)
                    //   有效 TTL = max(配置值, 3×本轮OCR耗时), 静止的号不会被误清。
                    let ttl_eff = ttl.max(3.0 * iter_secs);
                    // 空转退避: 连续无候选就把兜底间隔翻倍(上限 30s), 有候选立刻复位。
                    if cands.is_empty() {
                        empty_streak = empty_streak.saturating_add(1);
                        let k = 1u32 << empty_streak.min(4);
                        fb_eff = (fb_sec * k as f64).min(30.0);
                    } else {
                        empty_streak = 0;
                        fb_eff = fb_sec;
                    }
                    // TTS 预热: 第一轮看到候选就后台合成(结果落在常驻服务缓存里),
                    // 等第 stable_frames 轮确认通过再播时即命中缓存(~毫秒),
                    // 相当于把 1.8s 的合成耗时从关键路径上整体藏掉。
                    last_read_ok = !cands.is_empty();
                    for cand in &cands {
                        cand_raw.insert(cand.clone(), raw.clone());
                        if !announced.contains(cand) && prefetched.insert(cand.clone()) {
                            if prefetched.len() > 64 {
                                prefetched.clear();
                            }
                            ann.prefetch(&cand.0, &cand.1);
                        }
                    }
                    // 只对"尚未播报"的候选续观察期。若对已播报的号码也续期,
                    // 屏幕一直显示号码时观察期会被无限续期 -> OCR 变相常开白烧 CPU。
                    if cands.difference(&announced).next().is_some() {
                        active_until = now + active_sec;
                    }
                    // 清理超时未再出现的候选(闪变残留), 不立即清零 -> 短暂被替换的号码仍保留计数
                    confirm.retain(|_, v| now - v.1 <= ttl_eff);
                    let mut sorted: Vec<(String, String)> =
                        cands.iter().cloned().collect();
                    sorted.sort();
                    for cand in sorted {
                        let (cnt, first) = match confirm.get(&cand) {
                            Some(&(c, _, f)) => (c + 1, f),
                            None => (1, now), // 首次出现时刻 = 真正的"屏幕变化"起点
                        };
                        confirm.insert(cand.clone(), (cnt, now, first));
                        if cnt >= need_stable && !announced.contains(&cand) {
                            announced.insert(cand.clone());
                            logger::log(&cfg.log_file, &cand.0, &cand.1, &raw, "OK");
                            ann.speak(&cand.0, &cand.1); // 入FIFO队列, 播报线程按顺序播
                            println!(
                                "[时序] 首见→开始播报 {:.0} ms (确认{}轮, 本轮OCR {:.1}s); 另加相机/网络/轮询约 0.3~0.8 s",
                                (now - first) * 1000.0,
                                cnt,
                                iter_secs
                            );
                        }
                    }
                    if cands.is_empty() && changed {
                        logger::log(&cfg.log_file, "", "", &raw, "INVALID");
                    }
                }
                Err(e) => {
                    let iter_secs = now_secs() - iter_t0;
                    diag_ocr_n += 1;
                    diag_ocr_total += iter_secs;
                    if iter_secs > diag_ocr_max {
                        diag_ocr_max = iter_secs;
                    }
                    // 超时/失败同样计入空转退避, 别让病态帧把兜底间隔钉在基础值上
                    empty_streak = empty_streak.saturating_add(1);
                    let k = 1u32 << empty_streak.min(4);
                    fb_eff = (fb_sec * k as f64).min(30.0);
                    last_read_ok = false; // 这轮没读出内容, 屏上是什么是未知的
                    println!("[OCR失败] {}", e);
                }
            }
        }

        let rest = interval - (now_secs() - loop_start);
        std::thread::sleep(Duration::from_secs_f64(rest.max(0.05)));
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("[退出] {}", e);
        std::process::exit(1);
    }
}
