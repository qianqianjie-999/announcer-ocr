// capture.rs — 帧来源: simulate(读 sim_frame.jpg) / ffmpeg 管道(USB v4l2 与 RTSP 统一)
// 为什么走 ffmpeg 子进程: 不引任何 C 绑定(纯 Rust 依赖才能静态交叉编译),
// 且 ffmpeg 自带 v4l2/RTSP 解码与低延迟参数, 与 Python 版 OPENCV_FFMPEG_CAPTURE_OPTIONS 同源。
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub rgb: Vec<u8>, // rgb8, len = w*h*3
}

impl Frame {
    pub fn from_rgb8(w: usize, h: usize, rgb: Vec<u8>) -> Frame {
        Frame { w, h, rgb }
    }
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
}

pub struct PipeGrabber {
    latest: Arc<Mutex<Option<Arc<Frame>>>>,
    dead: Arc<Mutex<bool>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
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
    pub fn start(mut args: Vec<String>, w: usize, h: usize) -> std::io::Result<PipeGrabber> {
        args.push("-f".into());
        args.push("rawvideo".into());
        args.push("-pix_fmt".into());
        args.push("bgr24".into());
        args.push("-".into());
        // 首次同步启动: ffmpeg 未安装/参数错误仍立即报错(保留原有快速失败语义)
        // stdout 由抓帧线程统一 take(首次与重启路径共用同一套逻辑)
        let first = spawn_ffmpeg(&args)?;
        let latest = Arc::new(Mutex::new(None::<Arc<Frame>>));
        let dead = Arc::new(Mutex::new(false));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(first)));

        {
            let latest = latest.clone();
            let dead = dead.clone();
            let stop = stop.clone();
            let child_slot = child_slot.clone();
            thread::spawn(move || {
                use std::sync::atomic::Ordering::Relaxed;
                loop {
                    if stop.load(Relaxed) {
                        break;
                    }
                    // 取子进程: 槽里有就用(首次), 没有就重新 spawn
                    let mut child = loop {
                        {
                            let mut g = child_slot.lock().unwrap();
                            if let Some(c) = g.take() {
                                break c;
                            }
                        }
                        if stop.load(Relaxed) {
                            return;
                        }
                        match spawn_ffmpeg(&args) {
                            Ok(c) => {
                                println!("[抓帧] ffmpeg 已重新拉起");
                                break c;
                            }
                            Err(e) => {
                                println!("[抓帧] ffmpeg 重启失败({}), 5s 后再试", e);
                                thread::sleep(Duration::from_secs(5));
                            }
                        }
                    };
                    let stdout = child.stdout.take().expect("ffmpeg stdout");
                    // child 本体放回槽(供 Drop kill); stdout 已取走, 读帧不持锁
                    *child_slot.lock().unwrap() = Some(child);
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
                                *latest.lock().unwrap() = Some(f.clone());
                                *dead.lock().unwrap() = false;
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
                    {
                        let mut g = child_slot.lock().unwrap();
                        if let Some(mut c) = g.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                    }
                    if stop.load(Relaxed) {
                        break;
                    }
                    *dead.lock().unwrap() = true;
                    thread::sleep(Duration::from_secs(3));
                }
            });
        }
        Ok(PipeGrabber { latest, dead, stop, child: child_slot })
    }

    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.latest.lock().unwrap().clone()
    }

    pub fn is_dead(&self) -> bool {
        *self.dead.lock().unwrap()
    }
}

fn spawn_ffmpeg(args: &[String]) -> std::io::Result<Child> {
    Command::new("ffmpeg")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

impl Drop for PipeGrabber {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        self.stop.store(true, Relaxed);
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn to_rgb(bgr: &mut [u8]) {
    for px in bgr.chunks_exact_mut(3) {
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
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
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

/// 按 source 构造取帧通道。低延迟参数与 Python 版逐条对齐。
pub fn open(kind: &crate::cfg::SourceKind, cfg: &crate::cfg::Config) -> Result<Source, String> {
    let w = cfg.capture_width as usize;
    let h = cfg.capture_height as usize;
    match kind {
        crate::cfg::SourceKind::Simulate => Ok(Source::Simulate { path: "sim_frame.jpg".into() }),
        crate::cfg::SourceKind::Net(url) => {
            let mut args = vec![
                "-loglevel".into(), "error".into(),
                "-rtsp_transport".into(), "tcp".into(),
                "-i".into(), url.clone(),
                "-fflags".into(), "nobuffer".into(),
                "-flags".into(), "low_delay".into(),
                "-max_delay".into(), "300000".into(),
                "-an".into(),
                "-vf".into(), format!("scale={}:{}", w, h),
            ];
            extend_hwaccel(&mut args);
            let g = PipeGrabber::start(args, w, h)
                .map_err(|e| format!("ffmpeg 启动失败(需安装 ffmpeg): {}", e))?;
            thread::sleep(Duration::from_millis(1200)); // 等首帧
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
            let g = PipeGrabber::start(args, w, h)
                .map_err(|e| format!("ffmpeg 启动失败(设备 {}): {}", dev, e))?;
            thread::sleep(Duration::from_millis(1200));
            Ok(Source::Pipe(g))
        }
    }
}

/// 树莓派/J1850 这类弱机没有硬解码器时 ffmpeg 自动回落软解, 不加也没错;
/// 但显式关掉 vdpau/vaapi 探测可省 0.5s 启动时间。
fn extend_hwaccel(_args: &mut Vec<String>) {}
