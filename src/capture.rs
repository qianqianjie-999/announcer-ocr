// capture.rs — 帧来源: simulate(读 sim_frame.jpg) / ffmpeg 管道(USB v4l2 与 RTSP 统一)
// 为什么走 ffmpeg 子进程: 不引任何 C 绑定(纯 Rust 依赖才能静态交叉编译),
// 且 ffmpeg 自带 v4l2/RTSP 解码与低延迟参数, 与 Python 版 OPENCV_FFMPEG_CAPTURE_OPTIONS 同源。
use crate::util;
use std::io::{BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>, // rgb8, len = w*h*3
}

pub enum Source {
    Simulate { path: String },
    Pipe(PipeGrabber),
}

impl Source {
    /// 取最新一帧。simulate 每次重新读文件(测试台会原子替换);
    /// 管道模式取后台线程排空后的最新帧(不积压, 复刻 LatestFrame)。
    pub fn read(&mut self) -> Option<Arc<Frame>> {
        match self {
            Source::Simulate { path } => {
                let data = std::fs::read(path).ok()?;
                let img = image::load_from_memory(&data).ok()?;
                let rgb = img.to_rgb8().into_raw();
                let w = img.width() as usize;
                let h = img.height() as usize;
                Some(Arc::new(Frame { w, h, rgb }))
            }
            Source::Pipe(g) => g.latest(),
        }
    }

    /// 距最近一帧过了多少秒; simulate 模式恒为 None(每次 read 都现读文件)。
    /// 用途: [监测] 行打印 —— 帧龄异常增长就是"流僵死但进程还在"的直接证据。
    pub fn frame_age_sec(&self) -> Option<f64> {
        match self {
            Source::Simulate { .. } => None,
            Source::Pipe(g) => Some(g.frame_age_sec()),
        }
    }
}

pub struct PipeGrabber {
    latest: Arc<Mutex<Option<Arc<Frame>>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// 最近一次成功读到整帧的时刻(UNIX 毫秒)。看门狗据此判定"还在出帧吗"。
    /// 取 SystemTime 而非 Instant: Instant 塞不进 AtomicU64, 而这里只需粗粒度(秒级)
    /// 比较; 校时回拨最坏是少杀一次, 前跳最坏是多重拉一次, 都无害。
    last_frame_ms: Arc<std::sync::atomic::AtomicU64>,
    /// 抓帧线程句柄。Drop 需要**等它收完尸**再返回 —— 见 Drop 里的注释:
    /// 只把 stop 置位就返回, 主进程可能先退出, 那条线程手上的 ffmpeg 就变孤儿了。
    handle: Option<thread::JoinHandle<()>>,
    /// 无帧看门狗线程句柄(仅 stale_secs > 0 时存在)。同样要等它退出。
    wd_handle: Option<thread::JoinHandle<()>>,
}

impl PipeGrabber {
    /// 启动 ffmpeg 并起后台排空线程: 以满帧率读 rawvideo, 主循环永远取最新帧。
    /// 必要性(与 Python 版相同): 主循环只有 fps=3, 若不排空, 解码缓冲积压会让延迟持续上涨。
    ///
    /// ★自动重启(13:21 实测踩坑): 首次启动 ffmpeg 若因 /dev/video0 被上一个实例的
    ///   孤儿进程短暂占用而失败/秒退, 原实现的管道读取结束 => dead 永久置位,
    ///   主循环从此只刷 "[错误] 取帧失败" 直到人工重启服务。
    ///   现改为: 管道断开/子进程退出 => kill+wait 收尸(否则变 defunct 僵尸) =>
    ///   3s 后重新拉起 ffmpeg, 直到 stop(Drop) 为止。无人值守现场的自愈能力。
    ///
    /// ★★`stale_secs`: 无帧看门狗阈值(秒), 0 = 关闭(默认 60)。
    ///   为什么必须加这层(2026-10-22 补全最后一块拼图): 上面的自动重启只在
    ///   `read_exact` **返回 Err** 时才触发。而 RTSP 半开 —— TCP 没断、ffmpeg 进程在
    ///   ps 里好端端活着、CPU 也在跑, 就是不再吐帧 —— 时, `read_exact` 会永远阻塞,
    ///   既不 Ok 也不 Err, 于是永远不重拉: 主循环一直拿最后一帧(或坏帧), 帧差恒为 0,
    ///   **静默不播**, 而 systemctl / journal / ps 全显示"正常"。这是"机器正常却不播报"
    ///   的唯一硬风险。看门狗的做法是直接 kill 掉 ffmpeg: 管道写端随之关闭,
    ///   `read_exact` 立刻返回 UnexpectedEof, 剩下的交给上面那套既有重拉逻辑。
    pub fn start(
        mut args: Vec<String>,
        w: usize,
        h: usize,
        stale_secs: f64,
    ) -> std::io::Result<PipeGrabber> {
        args.push("-f".into());
        args.push("rawvideo".into());
        args.push("-pix_fmt".into());
        args.push("bgr24".into());
        args.push("-".into());
        // 首次同步启动: ffmpeg 未安装/参数错误仍立即报错(保留原有快速失败语义)
        // stdout 由抓帧线程统一 take(首次与重启路径共用同一套逻辑)
        let first = spawn_ffmpeg(&args)?;
        let latest = Arc::new(Mutex::new(None::<Arc<Frame>>));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(first)));
        // 起跑就打上时间戳: 首帧迟迟不来(流根本没建立)同样算"无帧", 交给看门狗重拉。
        let last_frame_ms = Arc::new(std::sync::atomic::AtomicU64::new(now_ms()));

        let handle = {
            let latest = latest.clone();
            let stop = stop.clone();
            let child_slot = child_slot.clone();
            let last_frame_ms = last_frame_ms.clone();
            thread::spawn(move || {
                use std::sync::atomic::Ordering::Relaxed;
                loop {
                    if stop.load(Relaxed) {
                        break;
                    }
                    // 取子进程: 槽里有就用(首次), 没有就重新 spawn
                    let mut child = loop {
                        {
                            let mut g = util::lock(&child_slot);
                            if let Some(c) = g.take() {
                                break c;
                            }
                        }
                        if stop.load(Relaxed) {
                            return;
                        }
                        match spawn_ffmpeg(&args) {
                            Ok(mut c) => {
                                // ★刚 spawn 完就收到停止请求: 当场收尸, 别留孤儿。
                                //   Drop 的等待窗口此时可能已经走完, 这是最后一个漏点。
                                if stop.load(Relaxed) {
                                    let _ = c.kill();
                                    let _ = c.wait();
                                    return;
                                }
                                println!("[抓帧] ffmpeg 已重新拉起");
                                break c;
                            }
                            Err(e) => {
                                println!("[抓帧] ffmpeg 重启失败({}), 5s 后再试", e);
                                thread::sleep(Duration::from_secs(5));
                            }
                        }
                    };
                    let stdout = match child.stdout.take() {
                        Some(s) => s,
                        // spawn 时固定了 Stdio::piped(), 不会为 None。真出现就收尸后
                        // 走"重拉"路径(与外层管道断开同构), 而不是 unwrap 崩进程。
                        None => {
                            println!("[抓帧] ffmpeg stdout 不可用, 3s 后重新拉起");
                            let _ = child.kill();
                            let _ = child.wait();
                            thread::sleep(Duration::from_secs(3));
                            continue;
                        }
                    };
                    // child 本体放回槽(供 Drop kill); stdout 已取走, 读帧不持锁
                    *util::lock(&child_slot) = Some(child);
                    let mut r = BufReader::with_capacity(1 << 20, stdout);
                    let frame_size = w * h * 3;
                    let mut bgr = vec![0u8; frame_size];
                    let mut n_frames: u64 = 0;
                    let t_start = std::time::Instant::now();
                    let mut last_log = std::time::Instant::now();
                    loop {
                        if stop.load(Relaxed) {
                            break;
                        }
                        match r.read_exact(&mut bgr) {
                            Ok(()) => {
                                let mut rgb = bgr.clone();
                                to_rgb(&mut rgb);
                                let f = Arc::new(Frame { w, h, rgb });
                                *util::lock(&latest) = Some(f.clone());
                                last_frame_ms.store(now_ms(), Relaxed);
                                n_frames += 1;
                                // 日志降噪: 原 60 帧一条会一天刷 4 万行(约7MB), 挤掉
                                // 有用日志还耗 SD 写寿命。改为每 10 分钟一条。
                                if last_log.elapsed().as_secs() >= 600 {
                                    last_log = std::time::Instant::now();
                                    println!(
                                        "[抓帧] 管道已读 {} 帧 (累计, 平均 {:.1} fps)",
                                        n_frames,
                                        n_frames as f64 / t_start.elapsed().as_secs_f64().max(1.0)
                                    );
                                }
                                // 预览按需落盘(必须在这里: 主循环可能正卡在慢 OCR 上)
                                serve_preview(&f, "抓帧线程");
                            }
                            Err(e) => {
                                println!(
                                    "[抓帧] ffmpeg 管道读取结束({}), 3s 后重新拉起",
                                    e
                                );
                                break;
                            }
                        }
                    }
                    // 收尸: kill + wait, 不留 defunct 僵尸(实测僵尸会一直挂在 ps 里)
                    // ★2026-10-22 中危修复: take 出来后立刻放锁再收尸 —— ffmpeg 收
                    //   SIGKILL 后可能进 D 状态(v4l2 读卡在僵死的 USB 摄像头上),
                    //   wait 会久等; 持锁 wait 会把看门狗与 Drop 的收尸全部堵死,
                    //   "RTSP 半开"唯一的救命层失效。slot 已 take 空, 不放回,
                    //   下面的重拉路径自会重建。与看门狗/reap_child 同一模式。
                    let victim = util::lock(&child_slot).take();
                    if let Some(mut c) = victim {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                    if stop.load(Relaxed) {
                        break;
                    }
                    thread::sleep(Duration::from_secs(3));
                }
            })
        };
        // 无帧看门狗: 唯一能救"RTSP 半开"的一层。它不重试、不接管重拉 ——
        // 只负责在超时后 kill 掉 ffmpeg, 让阻塞中的 read_exact 收到 EOF,
        // 交给上面那条线程既有的"3s 后重新拉起"路径处理, 逻辑不重复。
        let wd_handle = if stale_secs > 0.0 {
            let stop = stop.clone();
            let child_slot = child_slot.clone();
            let last_frame_ms = last_frame_ms.clone();
            Some(thread::spawn(move || {
                use std::sync::atomic::Ordering::Relaxed;
                // 每 0.5s 醒一次: 既能及时响应 stop, 也够粗(一次比较, 开销可忽略)。
                loop {
                    for _ in 0..10 {
                        if stop.load(Relaxed) {
                            return;
                        }
                        thread::sleep(Duration::from_millis(500));
                    }
                    if stop.load(Relaxed) {
                        return;
                    }
                    let now = now_ms();
                    let age = match stale_age_sec(last_frame_ms.load(Relaxed), now, stale_secs) {
                        Some(a) => a,
                        None => continue,
                    };
                    println!(
                        "[抓帧] 警告: 已 {:.0}s 没有新帧(疑似 RTSP 半开/流僵死), kill ffmpeg 触发重拉",
                        age
                    );
                    // kill + wait: 管道写端关闭 => 抓帧线程的 read_exact 返回 UnexpectedEof。
                    if let Some(mut c) = util::lock(&child_slot).take() {
                        let _ = c.kill();
                        let _ = c.wait();
                    }
                    // 重置时间戳: 重拉也需要时间, 别在下一帧到来前每 5s 刷一条警告。
                    last_frame_ms.store(now, Relaxed);
                }
            }))
        } else {
            println!("[抓帧] 无帧看门狗已关闭(frame_stale_sec=0)");
            None
        };

        Ok(PipeGrabber {
            latest,
            stop,
            child: child_slot,
            last_frame_ms,
            handle: Some(handle),
            wd_handle,
        })
    }

    pub fn latest(&self) -> Option<Arc<Frame>> {
        util::lock(&self.latest).clone()
    }

    /// 距最近一帧过了多少秒。主循环/诊断用它判断"画面是活的还是冻住的"。
    pub fn frame_age_sec(&self) -> f64 {
        use std::sync::atomic::Ordering::Relaxed;
        now_ms().saturating_sub(self.last_frame_ms.load(Relaxed)) as f64 / 1000.0
    }
}

/// 当前时刻(UNIX 毫秒)。看门狗与抓帧线程共用同一时基。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 无帧看门狗的判定核心(纯函数, 便于单测): 返回 Some(帧龄秒) 表示该 kill 重拉了。
/// - `stale_secs <= 0` => 恒 None(功能关闭);
/// - 帧龄 < 阈值 => None;
/// - **时钟回拨/异常时 saturating 到 0** => None, 宁可漏杀也不误杀(重拉有 3s 空窗)。
fn stale_age_sec(last_ms: u64, now_ms: u64, stale_secs: f64) -> Option<f64> {
    if stale_secs <= 0.0 {
        return None;
    }
    let age = now_ms.saturating_sub(last_ms) as f64 / 1000.0;
    (age >= stale_secs).then_some(age)
}

fn spawn_ffmpeg(args: &[String]) -> std::io::Result<Child> {
    Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

impl PipeGrabber {
    /// kill + wait 掉当前槽里的子进程(不管有没有)。幂等, 可反复调用。
    fn reap_child(&self) {
        if let Some(mut c) = util::lock(&self.child).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Drop for PipeGrabber {
    /// 收尸三段式(2026-10-11 补优雅退出时重写):
    ///   ① 置停止标志 —— 抓帧线程与主循环都会看到;
    ///   ② kill 槽里的子进程 —— 打断它阻塞中的 read_exact;
    ///   ③ **等抓帧线程自己收完尾**(有界 2s)。
    ///
    /// 为什么必须有 ③: 子进程在"被线程从槽里取走、还没放回槽"的那几行代码之间时,
    /// Drop 的 ② 会扑空 —— 而那条线程自己是会 kill+wait 的。不等它, 主进程一退出,
    /// 线程手上的 ffmpeg 就成了占住 /dev/video0 的孤儿 —— 这正是
    /// docs/07-troubleshooting.md 里"取帧失败先查孤儿 ffmpeg"那条的成因。
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.stop.store(true, Relaxed);
        self.reap_child();
        // 两条线程都等: 看门狗最多再睡 0.5s 就退出, 与抓帧线程共用同一个 2s 预算。
        for h in [self.handle.take(), self.wd_handle.take()].into_iter().flatten() {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while !h.is_finished() && std::time::Instant::now() < deadline {
                self.reap_child(); // 线程可能刚把子进程放回槽里, 补刀
                thread::sleep(Duration::from_millis(20));
            }
            if h.is_finished() {
                let _ = h.join();
            } else {
                self.reap_child();
                println!("[抓帧] 收尾超时(2s), 已强制收尸, 抓帧线程随进程退出");
            }
        }
    }
}

fn to_rgb(bgr: &mut [u8]) {
    // clippy 1.99 起 chunks_exact_mut(常量) 会被 chunks_exact_to_as_chunks 拦下,
    // 按建议改 as_chunks_mut::<3>: 每 3 字节一个 BGR 像素, 首尾交换成 RGB。
    for px in bgr.as_chunks_mut::<3>().0 {
        px.swap(0, 2);
    }
}

/// 从整帧裁 ROI 为 RgbImage(供 OCR/存证用), 越界自动钳制。
pub fn crop_rgb(frame: &Frame, roi: (usize, usize, usize, usize)) -> image::RgbImage {
    let (x, y, w, h) = roi;
    let w = w.min(frame.w.saturating_sub(x));
    let h = h.min(frame.h.saturating_sub(y));
    let mut img = image::RgbImage::new(w as u32, h as u32);
    for yy in 0..h {
        let s = ((y + yy) * frame.w + x) * 3;
        let d = yy * w * 3;
        img.as_mut()[d..d + w * 3].copy_from_slice(&frame.rgb[s..s + w * 3]);
    }
    img
}

/// Web 预览的缩图倍数。
/// ★为什么必须缩: 1080p 一帧的 JPEG 编码在这台 4 核赛扬上要 200ms 以上, 配置台只要
///   每秒刷一帧就吃掉近半核 —— 和 OCR、TTS 抢 CPU, 现场表现就是"开预览就卡"。
///   缩到 1/2(960×540) 后编码像素量降到 1/4、单帧 90KB→约 25KB, 浏览器解码和
///   drawImage 压力同步下降; 看画面瞄摄像头、拖 ROI 完全够用。
///   注意: 配置台前端已按「配置的采集分辨率」换算 ROI 坐标(见 config_web.py 的 capW()),
///   所以预览缩图不会改坏 ROI 数值。
const PREVIEW_DIV: usize = 2;

/// 生成预览用的缩图(div×div 盒子平均)。顺带省掉全尺寸帧的 6MB 克隆。
/// div<=1 时退化为直接复制原帧; 尺寸不合法返回 None。
fn downscale(frame: &Frame, div: usize) -> Option<image::RgbImage> {
    let w = frame.w;
    let h = frame.h;
    if div <= 1 {
        return image::RgbImage::from_raw(w as u32, h as u32, frame.rgb.clone());
    }
    if frame.rgb.len() < w * h * 3 {
        return None;
    }
    let (nw, nh) = (w / div, h / div);
    if nw < 16 || nh < 16 {
        return None;
    }
    let mut out = Vec::with_capacity(nw * nh * 3);
    let n = (div * div) as u32;
    for y in 0..nh {
        for x in 0..nw {
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            for dy in 0..div {
                let mut i = ((y * div + dy) * w + x * div) * 3;
                for _ in 0..div {
                    r += frame.rgb[i] as u32;
                    g += frame.rgb[i + 1] as u32;
                    b += frame.rgb[i + 2] as u32;
                    i += 3;
                }
            }
            out.push((r / n) as u8);
            out.push((g / n) as u8);
            out.push((b / n) as u8);
        }
    }
    image::RgbImage::from_raw(nw as u32, nh as u32, out)
}

/// 按需把最新帧落盘为 Web 预览图。
/// 约定: 配置台创建 `_live/req` 即请求一帧, 本函数写 `_live/latest.jpg` 后删除请求。
/// 空闲时零写盘(不耗 SD 卡寿命)。
/// ★必须先写 .tmp 再 rename 原子替换: 直接 create("latest.jpg") 的瞬间文件就已可见,
///   配置台轮询到 exists() 就读, 会拿到只有 8KB 的半张图(实测踩过, 预览花屏)。
/// pub 供主循环复用 —— 两处各写一份实现时, 主循环那份漏了原子写, 抢到请求就半截图。
pub fn serve_preview(frame: &Frame, who: &str) {
    // ★必须串行化: 抓帧线程与主循环都会调用本函数, 两者共用同一个 .tmp 路径。
    //   并发时先完成的一方 rename 走了 tmp, 另一方 rename 就报 ENOENT(os error 2),
    //   journal 里会刷出成片 "[预览失败] No such file or directory"。
    //   用 try_lock: 拿不到锁说明另一路正在写(同样能达成"给配置台一帧"的目的),
    //   直接返回即可, 不阻塞抓帧线程。
    static PREVIEW_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = match PREVIEW_LOCK.try_lock() {
        Ok(g) => g,
        Err(_) => return,
    };

    let req = "_live/req";
    if !std::path::Path::new(req).exists() {
        return;
    }
    let _ = std::fs::create_dir_all("_live");
    let tmp = "_live/latest.jpg.tmp";
    let dst = "_live/latest.jpg";
    let res = match downscale(frame, PREVIEW_DIV) {
        Some(img) => std::fs::File::create(tmp).and_then(|f| {
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(
                std::io::BufWriter::new(f),
                80,
            );
            enc.encode_image(&image::DynamicImage::ImageRgb8(img))
                .map_err(std::io::Error::other)
        }).and_then(|_| std::fs::rename(tmp, dst)),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "帧缓冲尺寸不匹配",
        )),
    };
    let _ = std::fs::remove_file(req);
    match res {
        Ok(()) => {
            if preview_log_due() {
                println!("[预览] 最新帧 -> {} ({})", dst, who);
            }
        }
        Err(e) => println!("[预览失败] {} ({})", e, who), // 失败一律留痕, 便于排障
    }
}

/// 预览成功日志节流: 首条必打, 之后每 120 秒最多一条。
/// 配置台开着「实时预览」时每秒请求一帧, 若逐条打日志, journal 一天涨 8 万行(约 15MB),
/// 会把真正有用的播报/OCR 日志挤出去。
fn preview_log_due() -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let last = LAST.load(Ordering::Relaxed);
    if last == 0 || now.saturating_sub(last) >= 120 {
        LAST.store(now, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// 首帧就绪的等待上限。
///
/// 为什么不再固定 `sleep(1200ms)`(评审 P2-6): 固定睡眠在"ffmpeg 冷启动快"时白等,
/// 在"RTSP 慢 / 相机刚上电"时又远远不够 —— 首帧没到就进主循环, 头几轮全是
/// "取帧失败"刷屏, 还看不出到底是没就绪还是真断了。改成轮询: 通常几百毫秒即返回,
/// 真正慢的源也给足时间(有上限, 不会无限等), 且"就绪/超时"都会留痕。
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(8);

/// 轮询 `ready()` 直到为真或超过 `timeout`, 返回是否在超时前就绪。
/// 抽成接受闭包的纯函数是为了可单测 —— 真实首帧依赖 ffmpeg/相机, 而"轮询到时/超时"
/// 这条控制流不该依赖环境。
fn poll_until(timeout: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let t0 = std::time::Instant::now();
    loop {
        if ready() {
            return true;
        }
        if t0.elapsed() >= timeout {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// 等 PipeGrabber 排出第一帧。
fn wait_first_frame(g: &PipeGrabber, timeout: Duration) -> bool {
    poll_until(timeout, || g.latest().is_some())
}

/// 首帧就绪/超时的统一留痕。超时不报错、不退出 —— 主循环自己会重试, 与旧行为一致。
fn report_ready(what: &str, ok: bool) {
    if ok {
        println!("[启动] {} 首帧就绪", what);
    } else {
        println!(
            "[启动] 警告: {} 等待首帧超时({}s), 先进主循环继续重试",
            what,
            FIRST_FRAME_TIMEOUT.as_secs()
        );
    }
}

/// 按 source 构造取帧通道。低延迟参数与 Python 版逐条对齐。
pub fn open(kind: &crate::cfg::SourceKind, cfg: &crate::cfg::Config) -> Result<Source, String> {
    let w = cfg.capture_width as usize;
    let h = cfg.capture_height as usize;
    match kind {
        crate::cfg::SourceKind::Simulate => Ok(Source::Simulate { path: "sim_frame.jpg".into() }),
        crate::cfg::SourceKind::Net(url) => {
            let mut args = vec!["-loglevel".into(), "error".into()];
            // ★只对 rtsp 源加 rtsp 传输参数: 对 http(s) 源, ffmpeg 会直接报
            //   "Option rtsp_transport not found" 并拒绝打开输入(实测), 结果是永远取不到
            //   帧。cfg.rs 把 http(s) 也归到 Net, 所以必须按 URL 前缀区分, 不能无条件加。
            if url.starts_with("rtsp://") {
                args.push("-rtsp_transport".into());
                args.push("tcp".into());
            }
            args.extend([
                "-i".into(), url.clone(),
                "-fflags".into(), "nobuffer".into(),
                "-flags".into(), "low_delay".into(),
                "-max_delay".into(), "300000".into(),
                "-an".into(),
                "-vf".into(), format!("scale={}:{}", w, h),
            ]);
            extend_hwaccel(&mut args);
            let g = PipeGrabber::start(args, w, h, cfg.frame_stale_sec)
                .map_err(|e| format!("ffmpeg 启动失败(需安装 ffmpeg): {}", e))?;
            report_ready("网络流", wait_first_frame(&g, FIRST_FRAME_TIMEOUT));
            Ok(Source::Pipe(g))
        }
        crate::cfg::SourceKind::Device(dev) => {
            let mut args = vec![
                "-loglevel".into(), "error".into(),
                "-f".into(), "v4l2".into(),
                "-input_format".into(), "mjpeg".into(),
                "-video_size".into(), format!("{}x{}", w, h),
                "-i".into(), dev.clone(),
                "-an".into(),
                "-vf".into(), format!("scale={}:{}", w, h),
            ];
            extend_hwaccel(&mut args);
            let g = PipeGrabber::start(args, w, h, cfg.frame_stale_sec)
                .map_err(|e| format!("ffmpeg 启动失败(设备 {}): {}", dev, e))?;
            report_ready(&format!("设备 {}", dev), wait_first_frame(&g, FIRST_FRAME_TIMEOUT));
            Ok(Source::Pipe(g))
        }
    }
}

/// 树莓派/J1850 这类弱机没有硬解码器时 ffmpeg 自动回落软解, 不加也没错;
/// 但显式关掉 vdpau/vaapi 探测可省 0.5s 启动时间。
fn extend_hwaccel(_args: &mut Vec<String>) {}

#[cfg(test)]
mod tests {
    use super::{crop_rgb, downscale, poll_until, stale_age_sec, Frame};
    use std::time::Duration;

    /// 构造 w×h 的帧, 每个像素填 (x, y, 常量) —— 便于断言"裁对了位置"。
    fn frame(w: usize, h: usize) -> Frame {
        let mut rgb = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                rgb.push(x as u8);
                rgb.push(y as u8);
                rgb.push(7);
            }
        }
        Frame { w, h, rgb }
    }

    #[test]
    fn crop_takes_exact_window() {
        let f = frame(8, 6);
        let c = crop_rgb(&f, (2, 1, 3, 2));
        assert_eq!((c.width(), c.height()), (3, 2));
        // 左上角应取自原图 (2,1)
        assert_eq!(c.get_pixel(0, 0).0, [2, 1, 7]);
        // 右下角 = (2+2, 1+1) = (4,2)
        assert_eq!(c.get_pixel(2, 1).0, [4, 2, 7]);
    }

    /// ROI 出界必须钳制成小图, 绝不能越界 panic(与 detect::prepare 的防御同源)。
    #[test]
    fn crop_clamps_out_of_frame_roi() {
        let f = frame(4, 4);
        let c = crop_rgb(&f, (3, 3, 100, 100));
        assert_eq!((c.width(), c.height()), (1, 1));
        assert_eq!(c.get_pixel(0, 0).0, [3, 3, 7]);

        // 完全出界 -> 0×0 空图(上层据此跳帧)
        let empty = crop_rgb(&f, (99, 99, 10, 10));
        assert_eq!((empty.width(), empty.height()), (0, 0));
    }

    #[test]
    fn downscale_by_two_averages_blocks() {
        // 用 40×40: 缩 2 倍后 20×20, 不低于 downscale 的 16px 下限
        let f = frame(40, 40);
        let d = downscale(&f, 2).expect("40x40 缩 2 倍应成功");
        assert_eq!((d.width(), d.height()), (20, 20));
        // 左上 2×2 块: x∈{0,1}, y∈{0,1} -> 平均 x=0.5->0, y=0.5->0
        assert_eq!(d.get_pixel(0, 0).0, [0, 0, 7]);
        // 右上块 x∈{2,3} -> 平均 2.5 -> 2; y∈{0,1} -> 0
        assert_eq!(d.get_pixel(1, 0).0, [2, 0, 7]);
    }

    #[test]
    fn downscale_div_one_copies_frame() {
        let f = frame(3, 2);
        let d = downscale(&f, 1).expect("div<=1 应退化为原尺寸复制");
        assert_eq!((d.width(), d.height()), (3, 2));
        assert_eq!(d.as_raw(), &f.rgb);
    }

    #[test]
    fn downscale_rejects_bad_input() {
        // 缩完小于 16px 的边 -> None(预览要能看清, 不做碎图)
        let small = frame(20, 20);
        assert!(downscale(&small, 2).is_none());
        // 缓冲区长度不足 -> None(尺寸不匹配时防越界读)
        let broken = Frame { w: 40, h: 40, rgb: vec![0u8; 10] };
        assert!(downscale(&broken, 2).is_none());
    }

    /// 轮询在就绪的那一刻立即返回, 且不多转一圈。
    #[test]
    fn poll_until_returns_as_soon_as_ready() {
        let mut n = 0;
        let ok = poll_until(Duration::from_secs(5), || {
            n += 1;
            n >= 3
        });
        assert!(ok, "第 3 次就绪时应返回 true");
        assert_eq!(n, 3, "就绪后不该再多轮询");
    }

    /// 永远不就绪时必须超时返回 false, 而不是死循环。
    #[test]
    fn poll_until_times_out() {
        let t0 = std::time::Instant::now();
        assert!(!poll_until(Duration::from_millis(150), || false));
        assert!(t0.elapsed() >= Duration::from_millis(150), "应至少等满超时时长");
        assert!(t0.elapsed() < Duration::from_secs(3), "不应明显超过超时时长");
    }

    // ———— 无帧看门狗 stale_age_sec: RTSP 半开是"进程活着却不播"的唯一硬风险 ————

    /// 正常出帧(15fps 约 0.07s 一帧)绝不触发重拉, 否则就是每 60s 白白断一次流。
    #[test]
    fn stale_not_triggered_on_healthy_stream() {
        let now = 1_700_000_000_000;
        assert_eq!(stale_age_sec(now - 70, now, 60.0), None, "0.07s 前的帧算新鲜");
        assert_eq!(stale_age_sec(now - 1_000, now, 60.0), None, "1s 前的帧算新鲜");
        assert_eq!(
            stale_age_sec(now - 59_000, now, 60.0),
            None,
            "差一点就到阈值也不该触发"
        );
    }

    /// 现场场景: 半开后 120s 无帧 —— 必须触发, 且带回帧龄便于日志定位。
    #[test]
    fn stale_triggers_when_stream_frozen() {
        let now = 1_700_000_000_000;
        let age = stale_age_sec(now - 120_000, now, 60.0);
        assert!(age.is_some(), "僵死 120s 必须触发重拉");
        assert!((age.unwrap() - 120.0).abs() < 0.5, "返回的帧龄应约为 120s");
    }

    /// 阈值 0 = 功能关闭(向后兼容老配置, 且给现场留手動关掉的口子)。
    #[test]
    fn stale_disabled_when_zero() {
        let now = 1_700_000_000_000;
        assert_eq!(stale_age_sec(now - 999_999, now, 0.0), None);
        assert_eq!(stale_age_sec(now - 999_999, now, -1.0), None);
    }

    /// 时钟回拨(校时/ntp 跳变)时 saturating 到 0, 宁可漏杀也不能误杀。
    #[test]
    fn stale_survives_clock_going_backwards() {
        let now = 1_700_000_000_000;
        assert_eq!(
            stale_age_sec(now + 500_000, now, 60.0),
            None,
            "now 早于 last 时帧龄按 0 算, 不触发"
        );
    }
}
