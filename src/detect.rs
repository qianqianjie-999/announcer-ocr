// detect.rs — 帧差变化检测（逐行复刻 announce.py 算法）
// 灰度 -> 缩到 min(宽,480) -> 高斯模糊 -> (亮度漂移对齐) -> absdiff 超阈占比。
// 为什么先对齐亮度再比: 摄像头自动曝光持续微调整帧亮度, 原始 absdiff 会把漂移
// 当成"画面变化"导致空场景反复触发 OCR; 对齐后阈值才能压到 0.0015 不漏检。
use crate::capture::Frame;
use image::imageops::{resize, FilterType};
use image::{GrayImage, Luma};

pub struct Prepared {
    pub gray: GrayImage,
}

/// 从整帧裁 ROI 并做检测前处理。
/// ROI 完全出界时返回 0×0 空图(调用方须跳帧): RTSP 重连/降流会临时改变帧尺寸,
/// USB 恒定分辨率从未暴露此路径, 网络源若不防御将数组越界 panic 杀死服务(实测)。
pub fn prepare(frame: &Frame, roi: (usize, usize, usize, usize)) -> Prepared {
    let (rx, ry, rw, rh) = roi;
    let rw = rw.min(frame.w.saturating_sub(rx));
    let rh = rh.min(frame.h.saturating_sub(ry));
    if rw == 0 || rh == 0 {
        return Prepared { gray: GrayImage::new(0, 0) };
    }
    let tw = rw.min(480);
    let th = std::cmp::max(1, rh * tw / rw.max(1));

    let mut gray = GrayImage::new(tw as u32, th as u32);
    for yy in 0..th {
        let src_row = (ry + yy) * frame.w;
        let scale = rw as f32 / tw as f32;
        for xx in 0..tw {
            // 面积采样: 对应缩放位置取邻近像素均值(与 OpenCV INTER_AREA 等价性足够)
            let x0 = ((xx as f32) * scale) as usize;
            let x1 = std::cmp::min(rw, (((xx + 1) as f32) * scale) as usize).max(x0 + 1);
            let mut acc = 0u32;
            for px in x0..x1 {
                let i = (src_row + rx + px) * 3;
                let g = (frame.rgb[i] as u32 * 299
                    + frame.rgb[i + 1] as u32 * 587
                    + frame.rgb[i + 2] as u32 * 114)
                    / 1000;
                acc += g;
            }
            gray.put_pixel(xx as u32, yy as u32, Luma([(acc / (x1 - x0) as u32) as u8]));
        }
    }
    Prepared { gray }
}

/// 高斯模糊 5x5(可分离核 [1,4,6,4,1]/16), 等价 cv2.GaussianBlur((5,5),0) 的平滑量级。
pub fn blur5(img: &mut GrayImage) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w < 5 || h < 5 {
        return;
    }
    let k = [1i32, 4, 6, 4, 1];
    let src = img.clone();
    // 横向
    let mut tmp = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0i32;
            for (i, &kv) in k.iter().enumerate() {
                let xx = (x as i32 + i as i32 - 2).clamp(0, w as i32 - 1) as usize;
                acc += src.get_pixel(xx as u32, y as u32)[0] as i32 * kv;
            }
            tmp[y * w + x] = (acc / 16) as u8;
        }
    }
    // 纵向
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0i32;
            for (i, &kv) in k.iter().enumerate() {
                let yy = (y as i32 + i as i32 - 2).clamp(0, h as i32 - 1) as usize;
                acc += tmp[yy * w + x] as i32 * kv;
            }
            img.put_pixel(x as u32, y as u32, Luma([(acc / 16) as u8]));
        }
    }
}

/// 返回 Some(ratio): 两帧同尺寸时算变化占比; None 表示首帧/尺寸变了(下一轮才可比)。
/// 内部先做亮度漂移对齐再比, 与 announce.py 完全一致。
pub fn change_ratio(cur: &GrayImage, prev: &GrayImage, diff_thr: f64) -> Option<f64> {
    let (cw, ch) = cur.dimensions();
    if prev.dimensions() != (cw, ch) {
        return None;
    }
    let n = (cw * ch) as f64;
    let cmean: f64 = cur.pixels().map(|p| p[0] as f64).sum::<f64>() / n;
    let pmean: f64 = prev.pixels().map(|p| p[0] as f64).sum::<f64>() / n;
    let shift = cmean - pmean;

    let mut hit = 0u64;
    for (x, y, pc) in cur.enumerate_pixels() {
        let c = pc[0] as f64;
        let p = prev.get_pixel(x, y)[0] as f64;
        let p2 = if shift.abs() >= 1.0 {
            (p + shift).clamp(0.0, 255.0)
        } else {
            p
        };
        if (c - p2).abs() > diff_thr {
            hit += 1;
        }
    }
    Some(hit as f64 / n)
}

/// OCR 增强预处理: 2x 放大 -> 取红通道 -> Otsu 自动阈值二值化 -> 反相(白底黑字)。
/// ★为什么是红通道: 叫号屏/测试卡都是"白字深色底", 白字三通道都高、深蓝底只有
///   红通道最低 —— 取红通道等价于把"字/底"对比拉到最大(灰度会被蓝底抬高, 实测
///   灰度版输出明显更差)。二值化必须有: 不加阈值的增强图里"号窗口"仍糊成
///   "写窗", 正则套不上(靶机实测两种都试过, 差别就在这一步)。
/// 靶机实测(401x180 ROI, 摄像头拍打印测试卡):
///   原图 psm11 -> 全乱码; 本增强 psm11 -> 两张卡的号码+窗口全部可解析。
pub fn enhance_for_ocr(img: &image::RgbImage) -> image::RgbImage {
    let up = resize(img, img.width() * 2, img.height() * 2, FilterType::CatmullRom);
    let (w, h) = up.dimensions();
    let n = (w * h) as usize;
    // 红通道灰度(白字高、深色底低)
    let mut g: Vec<u8> = Vec::with_capacity(n);
    for px in up.pixels() {
        g.push(px[0]);
    }
    // Otsu 大津法自动阈值(直方图类间方差最大化)
    let mut hist = [0u64; 256];
    for &v in &g {
        hist[v as usize] += 1;
    }
    let total = n as u64;
    let mut sum_all = 0f64;
    for (i, &h) in hist.iter().enumerate() {
        sum_all += i as f64 * h as f64;
    }
    let (mut sum_b, mut wb, mut best, mut thr) = (0f64, 0u64, 0f64, 127u8);
    for (t, &h) in hist.iter().enumerate() {
        wb += h;
        if wb == 0 {
            continue;
        }
        let wf = total - wb;
        if wf == 0 {
            break;
        }
        sum_b += t as f64 * h as f64;
        let mb = sum_b / wb as f64;
        let mf = (sum_all - sum_b) / wf as f64;
        let between = wb as f64 * wf as f64 * (mb - mf) * (mb - mf);
        if between > best {
            best = between;
            thr = t as u8;
        }
    }
    // 反相二值化: 字(亮, >thr) -> 0 黑, 底(暗) -> 255 白 —— tesseract 偏好白底黑字
    for v in g.iter_mut() {
        *v = if *v > thr { 0 } else { 255 };
    }
    // from_raw 仅在缓冲区长度与 w*h 不符时返回 None —— 本函数里 g 恰为 w*h 个像素,
    // 不可能发生。真发生就退回原图(不增强): 上层对"增强图读不出"本就有回退语义,
    // 崩掉整个进程才是不可接受的。
    match image::GrayImage::from_raw(w, h, g) {
        Some(gray) => image::DynamicImage::ImageLuma8(gray).to_rgb8(),
        None => img.clone(),
    }
}

/// 一行的"底色亮度"(红通道中位数)。用于判定三行列表里哪一行是浅色高亮行。
/// ★为什么用红通道: 高亮行是浅蓝底, 蓝分量高、红分量不低; 深蓝底行的红分量很低,
///   红通道能把两者的差距拉得比灰度更大(现场实测 RGB 均值蓝底红通道仅 30~60)。
pub fn median_luma_red(img: &image::RgbImage) -> u8 {
    let n = (img.width() * img.height()) as u64;
    if n == 0 {
        return 0;
    }
    let mut hist = [0u64; 256];
    for px in img.pixels() {
        hist[px[0] as usize] += 1;
    }
    let target = (n / 2).max(1);
    let mut acc = 0u64;
    for (v, &h) in hist.iter().enumerate() {
        acc += h;
        if acc >= target {
            return v as u8;
        }
    }
    255
}

/// 从各行底色亮度里挑出"高亮行"下标。
///
/// ★判据要用**相对比较**(最亮 vs 次亮), 不能用绝对阈值: 摄像头自动曝光整体漂移
///   时, 所有行亮度会一起涨落, 绝对阈值要么常亮要么常灭。
///   实测现场两档反差: 高亮行 153 vs 次亮 108(灰度), 比值 1.42、差 45;
///   阈值取 1.30 倍 + 至少差 20 能容下曝光波动, 又不会被普通深蓝行误判。
///   行数 <2(单行配置)时无从比较, 返回 None 保持旧链路。
pub fn pick_highlight_row(lumas: &[u8]) -> Option<usize> {
    if lumas.len() < 2 {
        return None;
    }
    let mut order: Vec<usize> = (0..lumas.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(lumas[i]));
    let (hi, second) = (order[0], order[1]);
    let (a, b) = (lumas[hi] as f32, lumas[second] as f32);
    if a >= b * 1.30 && a - b >= 20.0 {
        Some(hi)
    } else {
        None
    }
}

/// 直方图分位数(值域 0~255), 用于行内对比度拉伸的 lo/hi 端点。
fn percentile_u8(hist: &[u64; 256], total: u64, p: f64) -> f32 {
    let target = ((total as f64) * p / 100.0).ceil().max(1.0) as u64;
    let mut acc = 0u64;
    for (v, &h) in hist.iter().enumerate() {
        acc += h;
        if acc >= target {
            return v as f32;
        }
    }
    255.0
}

/// 3x3 中值滤波(边界按边缘复制, 与 PIL MedianFilter 一致)。
fn median3(src: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut win = [0u8; 9];
            let mut k = 0;
            for dy in 0..3usize {
                let yy = (y + dy).saturating_sub(1).min(h - 1);
                for dx in 0..3usize {
                    let xx = (x + dx).saturating_sub(1).min(w - 1);
                    win[k] = src[yy * w + xx];
                    k += 1;
                }
            }
            win.sort_unstable();
            out[y * w + x] = win[4];
        }
    }
    out
}

/// k×k 局部均值(可分离, 边界按边缘复制), 供自适应阈值用。
fn box_mean(src: &[u8], w: usize, h: usize, k: usize) -> Vec<f32> {
    let r = (k / 2) as isize;
    let inv = 1.0 / ((k * k) as f32);
    let mut tmp = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0u32;
            for d in -r..=r {
                let xx = (x as isize + d).clamp(0, w as isize - 1) as usize;
                acc += src[y * w + xx] as u32;
            }
            tmp[y * w + x] = acc as f32;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0f32;
            for d in -r..=r {
                let yy = (y as isize + d).clamp(0, h as isize - 1) as usize;
                acc += tmp[yy * w + x];
            }
            out[y * w + x] = acc * inv;
        }
    }
    out
}

/// 高亮行(浅蓝底白字)专用增强: 2x 放大 -> 红通道 -> 行内 2%/99% 分位拉伸
/// -> 3x3 中值 -> 局部均值自适应阈值 -> 反相(白底黑字)。
///
/// ★为什么现有 `enhance_for_ocr` 在高亮行上是**负收益**(2026-10-27 实测):
///   那条链用整行一个全局 Otsu 阈值。高亮行这一条 crop 里同时有"深蓝行缝、
///   浅蓝底、白字"三种亮度, Otsu 阈值落在前两者之间 -> **浅蓝底被判成字**,
///   二值化后 61% 像素连成一片黑, 字和底粘死。A/B 实测: 原图 psm6 能读对
///   42%, 过一遍旧增强反而掉到 0/10。
/// ★为什么是这套参数: 在高亮行上做 12 组预处理 A/B(2x/3x × 窗口 31/51/61 ×
///   C 8/12/15 × 是否中值), 用生产同款 tesseract 与生产正则判定, 该组合在两个
///   独立时段各 10/10、12/12 全对; 其他组合最好 9/10, 去掉中值或改用全局 Otsu
///   直接跌到 0~5/10。分位拉伸解决"局部对比只有 79(深底行是 140)", 中值去
///   摄像头噪点, 局部均值自适应则让阈值跟着每一小块的底色走, 不再被行缝带偏。
/// ★只能用在最亮那一行: 同一套算法套到深蓝底行, 识别率反而从 100% 掉到 83%
///   ("到"被读成"下/台"), 所以必须按行判别, 不能全表统一。
pub fn enhance_highlight_for_ocr(img: &image::RgbImage) -> image::RgbImage {
    let up = resize(img, img.width() * 2, img.height() * 2, FilterType::Lanczos3);
    let (w, h) = (up.width() as usize, up.height() as usize);
    let n = w * h;
    if n == 0 {
        return img.clone();
    }
    let mut g: Vec<u8> = Vec::with_capacity(n);
    for px in up.pixels() {
        g.push(px[0]);
    }
    // 行内分位拉伸: lo 取 2% 分位、hi 取 99% 分位, 把整行对比拉到满量程。
    // 用分位而不是 min/max, 是防个别反光亮点/死黑把动态范围吃掉。
    let mut hist = [0u64; 256];
    for &v in &g {
        hist[v as usize] += 1;
    }
    let lo = percentile_u8(&hist, n as u64, 2.0);
    let hi = percentile_u8(&hist, n as u64, 99.0);
    let span = (hi - lo).max(1.0);
    for v in g.iter_mut() {
        let t = ((*v as f32 - lo) * 255.0 / span).clamp(0.0, 255.0);
        *v = t as u8;
    }
    let med = median3(&g, w, h);
    let boxm = box_mean(&med, w, h, 31);
    let mut out = vec![0u8; n];
    for i in 0..n {
        // 阈值 = 局部均值 - 8, 上限 200: 留出 8 级余量压掉底色起伏,
        // 上限 200 防止某块整体偏暗时把白字(<255)也判成底色;
        // 下限 1 防"整块纯 0"的暗区把阈值压成负数、全块判黑(A/B 脚本里没这一条,
        // 实拍帧靠噪点躲过去了, 但纯色块会让 tesseract 版面切分变形)。
        let thr = (boxm[i] - 8.0).clamp(1.0, 200.0);
        out[i] = if med[i] as f32 > thr { 0 } else { 255 };
    }
    match image::GrayImage::from_raw(w as u32, h as u32, out) {
        Some(gray) => image::DynamicImage::ImageLuma8(gray).to_rgb8(),
        None => img.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(pix: &[u8], w: u32, h: u32) -> GrayImage {
        GrayImage::from_raw(w, h, pix.to_vec()).unwrap()
    }

    #[test]
    fn change_ratio_uniform_brightening_ignored() {
        // 整帧亮度漂移(自动曝光)对齐后应视为无变化
        let a = gray(&[100u8; 64], 8, 8);
        let b = gray(&[160u8; 64], 8, 8);
        let r = change_ratio(&a, &b, 0.05).unwrap();
        assert_eq!(r, 0.0);
    }

    #[test]
    fn prepare_roi_out_of_frame_returns_empty_no_panic() {
        // RTSP 重连/降流后帧变小、ROI 出界: 必须返回空图而非越界 panic(2026-10-07 实测炸过)
        let mut f = Frame { w: 640, h: 360, rgb: vec![128u8; 640 * 360 * 3] };
        assert_eq!(prepare(&f, (482, 396, 1108, 560)).gray.width(), 0); // 完全出界
        assert_eq!(prepare(&f, (0, 0, 1920, 1080)).gray.width(), 480); // ROI 超大帧→夹到帧内
        assert_eq!(prepare(&f, (600, 340, 700, 400)).gray.width(), 40); // 半出界→裁剪存活
        assert_eq!(prepare(&f, (640, 360, 700, 400)).gray.width(), 0); // 边界贴齐=出界
        // 尺寸恢复正常后必须能正常出图(服务自愈验证)
        f.w = 960; f.h = 720; f.rgb = vec![128u8; 960 * 720 * 3];
        assert!(prepare(&f, (482, 396, 1108, 560)).gray.width() > 0);
    }

    #[test]
    fn change_ratio_real_change_detected() {
        // 均值不变(上抬/下压互相抵消)的局部变化: 16/64 像素改变 -> 0.25
        let a = gray(&[160u8; 64], 8, 8);
        let mut pix = vec![160u8; 64];
        for v in pix.iter_mut().take(8) {
            *v = 255;
        }
        for v in pix.iter_mut().skip(8).take(8) {
            *v = 65;
        }
        let r = change_ratio(&a, &gray(&pix, 8, 8), 0.05).unwrap();
        assert!((r - 0.25).abs() < 1e-9);
    }

    #[test]
    fn change_ratio_size_mismatch_is_none() {
        let a = gray(&[100u8; 64], 8, 8);
        let b = gray(&[100u8; 36], 6, 6);
        assert!(change_ratio(&a, &b, 0.05).is_none());
    }

    #[test]
    fn enhance_binary_output_and_otsu_separates() {
        // 左半黑底(字"不存在"), 右半白块: 增强后应输出纯二值(只含 0/255)
        let mut pix = vec![10u8; 32 * 16 * 3];
        for x in 16..32 {
            for y in 0..16 {
                let i = ((y * 32 + x) * 3) as usize;
                pix[i] = 240;
                pix[i + 1] = 240;
                pix[i + 2] = 240;
            }
        }
        let img = image::RgbImage::from_raw(32, 16, pix).unwrap();
        let out = enhance_for_ocr(&img);
        assert!(out.pixels().all(|p| p[0] == 0 || p[0] == 255));
    }

    #[test]
    fn pick_highlight_row_needs_clear_contrast() {
        // 现场实测三行底色(红通道近似): 30 / 150 / 45 -> 高亮是第 2 行
        assert_eq!(pick_highlight_row(&[30, 150, 45]), Some(1));
        // 三行亮度接近(无高亮行)时不启用, 免得把深色行送进高亮专用链路
        assert_eq!(pick_highlight_row(&[30, 40, 35]), None);
        // 只亮一点点(1.15 倍)也不算 —— 自动曝光漂移会造假高亮
        assert_eq!(pick_highlight_row(&[100, 115, 90]), None);
        // 单行配置无从比较, 保持旧链路
        assert_eq!(pick_highlight_row(&[150]), None);
        assert_eq!(pick_highlight_row(&[]), None);
    }

    #[test]
    fn enhance_highlight_separates_light_bg_white_text() {
        // 浅蓝底(红通道 200) + 白字块(250): 模拟高亮行。旧链路的全局 Otsu 会
        // 把浅底判成字; 新链路必须让白字变黑、浅底变白。
        // ★底色不要掺深色行缝再取 2% 分位 —— 合成图里若深缝占比超过 2%, 分位
        //   端点会跑到深缝上, 底色被拉伸成中灰, 与真实行2(底色就是最暗端)不符。
        let (w, h) = (40usize, 20usize);
        let mut pix = vec![200u8; w * h * 3];
        for y in 6..14 {
            for x in 10..30 {
                let i = (y * w + x) * 3;
                pix[i] = 250;
                pix[i + 1] = 250;
                pix[i + 2] = 250;
            }
        }
        let img = image::RgbImage::from_raw(w as u32, h as u32, pix).unwrap();
        let out = enhance_highlight_for_ocr(&img);
        assert_eq!(out.dimensions(), (w as u32 * 2, h as u32 * 2));
        assert!(out.pixels().all(|p| p[0] == 0 || p[0] == 255), "必须纯二值");
        // 放大 2x 后: 白字块占 x 20..60, y 12..28
        let ink = out.get_pixel(40, 20)[0];
        let paper = out.get_pixel(70, 20)[0];
        assert_eq!(ink, 0, "白字处应为墨");
        assert_eq!(paper, 255, "浅蓝底应为纸(旧链路在这里会判反)");
    }
}
