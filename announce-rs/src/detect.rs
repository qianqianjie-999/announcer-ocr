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
pub fn prepare(frame: &Frame, roi: (usize, usize, usize, usize)) -> Prepared {
    let (rx, ry, rw, rh) = roi;
    let rw = rw.min(frame.w.saturating_sub(rx));
    let rh = rh.min(frame.h.saturating_sub(ry));
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

/// 仅用于心跳展示的缩放辅助(与 prepare 无关)。
pub fn resize_for_ocr(img: &image::RgbImage, fx: f32) -> image::RgbImage {
    let (w, h) = img.dimensions();
    let nw = (w as f32 * fx) as u32;
    let nh = (h as f32 * fx) as u32;
    resize(img, nw.max(1), nh.max(1), FilterType::CatmullRom)
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
    for i in 0..256usize {
        sum_all += i as f64 * hist[i] as f64;
    }
    let (mut sum_b, mut wb, mut best, mut thr) = (0f64, 0u64, 0f64, 127u8);
    for t in 0..256usize {
        wb += hist[t];
        if wb == 0 {
            continue;
        }
        let wf = total - wb;
        if wf == 0 {
            break;
        }
        sum_b += t as f64 * hist[t] as f64;
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
    let gray = image::GrayImage::from_raw(w, h, g).expect("enhance: gray buffer");
    image::DynamicImage::ImageLuma8(gray).to_rgb8()
}
