# -*- coding: utf-8 -*-
"""check_changed_ratio.py — 估算"屏幕换号"时的帧差比例, 验证 changed_ratio 阈值是否留余量。

为什么需要它: 阈值调高能压掉相机噪声(实测 40 秒 46 条 INVALID -> 5 分钟 0 条),
但调过头就会漏掉真实的换号。这个脚本用两张测试卡离线估算最坏情况:
    A 卡 -> B 卡, 两张都是白底黑字, 只有文字不同, 变化区域最小。
按不同占屏比例模拟举卡远近, 再完全复刻 announce.py 的检测算法算 changed_ratio。

判读: 算出来的值应明显大于 config.detect.changed_ratio, 否则漏检风险高。
      (阈值不写死在这里, 直接读 config.json, 避免脚本与程序对不上。)
"""
import json
import os

import cv2
import numpy as np

W, H = 1920, 1080


def frame_with_card(card_path, occupy, canvas=None):
    """把竖屏卡按 occupy 比例(占画面高度)贴到 1920x1080 画布中央。"""
    bg = np.full((H, W, 3), 120, np.uint8) if canvas is None else canvas.copy()
    img = cv2.imread(card_path)
    if img is None:
        raise SystemExit("读不到 " + card_path)
    ch = int(H * occupy)
    cw = int(img.shape[1] * ch / img.shape[0])
    card = cv2.resize(img, (cw, ch), interpolation=cv2.INTER_AREA)
    x0, y0 = (W - cw) // 2, (H - ch) // 2
    if x0 < 0:                       # 卡太宽, 按宽度重新贴合
        cw = int(W * 0.9)
        ch = int(img.shape[0] * cw / img.shape[1])
        card = cv2.resize(img, (cw, ch), interpolation=cv2.INTER_AREA)
        x0, y0 = (W - cw) // 2, (H - ch) // 2
    bg[y0:y0 + ch, x0:x0 + cw] = card
    return bg


def changed_ratio(a, b, thr=25):
    """完全复刻 announce.py 的帧差算法(整帧当ROI)。"""
    def prep(f):
        g = cv2.cvtColor(f, cv2.COLOR_BGR2GRAY)
        g = cv2.resize(g, (480, max(1, int(H * 480 / W))))
        return cv2.GaussianBlur(g, (5, 5), 0)
    ga, gb = prep(a), prep(b)
    return float((cv2.absdiff(ga, gb) > thr).mean()), ga, gb


ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
cfg = json.load(open(os.path.join(ROOT, "config.json"), encoding="utf-8"))
THR = cfg["detect"]["changed_ratio"]
CARDS = [(os.path.join(ROOT, "assets", "test_card.jpg"),
          os.path.join(ROOT, "assets", "test_card_B.jpg"))]

print("=" * 76)
print("帧差比例估算 (config.detect.changed_ratio 阈值 = %.4f)" % THR)
print("=" * 76)
print("%-12s %-14s %-12s %-10s" % ("占屏高度", "场景", "changed_ratio", "判定"))

for occupy in (0.85, 0.65, 0.45, 0.30, 0.20):
    a = frame_with_card(CARDS[0][0], occupy)
    b = frame_with_card(CARDS[0][1], occupy)
    r_swap, _, _ = changed_ratio(a, b)
    blank = np.full((H, W, 3), 120, np.uint8)
    r_appear, _, _ = changed_ratio(blank, a)
    for name, r in (("A卡->B卡(仅文字变)", r_swap), ("空场->A卡(出现)", r_appear)):
        verdict = "安全(余量 %.1fx)" % (r / THR) if r > THR * 1.5 else (
            "偏低(仅 %.1fx)" % (r / THR) if r > THR else "会漏检!")
        print("%-12s %-14s %-12.4f %-10s" % ("%.0f%%" % (occupy * 100), name, r, verdict))
    print()

# 反向确认: 相机静态噪声大概什么量级 -> 用同一画布加正态噪声模拟
print("-" * 76)
print("对照组: 同一画面叠加传感器噪声后的 changed_ratio 量级")
print("=" * 76)
base = frame_with_card(CARDS[0][0], 0.65).astype(np.float32)
for sigma in (1.0, 2.0, 4.0, 8.0):
    noisy = np.clip(base + np.random.normal(0, sigma, base.shape), 0, 255).astype(np.uint8)
    r, _, _ = changed_ratio(frame_with_card(CARDS[0][0], 0.65), noisy)
    print("噪声 sigma=%-4.1f -> changed_ratio=%.4f  %s"
          % (sigma, r, "高于阈值(会误触发)" if r > THR else "低于阈值(不触发, 正确)"))

# ---------------------------------------------------------------- 真实部署场景
print()
print("=" * 76)
print("真实部署场景: 相机对准屏幕不动, 屏上只有号码在变 -> ROI 收紧到文字区域")
print("(上面」整帧当ROI」的结果不可直接套用: 整帧缩到 480 宽后,")
print(" 文字笔画被抹平, 帧差自然接近 0; 真实部署 ROI 是紧贴文字的, 不存在这个问题)")
print("=" * 76)


def text_bbox(img, thr=100):
    """找黑色文字的包围盒(测试卡是白底黑字)。"""
    g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    ys, xs = np.where(g < thr)
    if len(xs) == 0:
        return None
    return int(xs.min()), int(ys.min()), int(xs.max()), int(ys.max())


def roi_ratio(a, b, target_w=480):
    """完全复刻 announce.py 的 ROI 算法: 灰度 -> 缩到 min(宽,480) -> 高斯模糊 -> 占比。"""
    def prep(f):
        g = cv2.cvtColor(f, cv2.COLOR_BGR2GRAY)
        w = g.shape[1]
        tw = min(w, target_w)
        g = cv2.resize(g, (tw, max(1, int(g.shape[0] * tw / w))))
        return cv2.GaussianBlur(g, (5, 5), 0)
    ga, gb = prep(a), prep(b)
    return float((cv2.absdiff(ga, gb) > 25).mean())


A = cv2.imread(CARDS[0][0])
B = cv2.imread(CARDS[0][1])
bb = text_bbox(A)
if bb:
    x0, y0, x1, y1 = bb
    pad = 24
    x0, y0 = max(0, x0 - pad), max(0, y0 - pad)
    x1, y1 = min(A.shape[1], x1 + pad), min(A.shape[0], y1 + pad)
    ra, rb = A[y0:y1, x0:x1], B[y0:y1, x0:x1]
    r_all = roi_ratio(ra, rb)
    print("文字区域尺寸          = %d x %d px" % (x1 - x0, y1 - y0))
    print("整段文字 A->B         = %.4f  %s"
          % (r_all, "安全(余量 %.1fx)" % (r_all / THR) if r_all > THR else "会漏检!"))

    # 只变一行(更接近真实: 有时屏上只跳一行)
    mask = (cv2.cvtColor(ra, cv2.COLOR_BGR2GRAY) < 100).sum(axis=1)
    rows = np.where(mask > 0)[0]
    if len(rows) > 0:
        gaps = np.where(np.diff(rows) > 8)[0]
        if len(gaps) > 0:
            cut = rows[gaps[0]] + (rows[gaps[0] + 1] - rows[gaps[0]]) // 2
            r_l1 = roi_ratio(ra[:cut], rb[:cut])
            r_l2 = roi_ratio(ra[cut:], rb[cut:])
            print("仅第一行文字 A->B     = %.4f  %s"
                  % (r_l1, "安全(余量 %.1fx)" % (r_l1 / THR) if r_l1 > THR else "会漏检!"))
            print("仅第二行文字 A->B     = %.4f  %s"
                  % (r_l2, "安全(余量 %.1fx)" % (r_l2 / THR) if r_l2 > THR else "会漏检!"))

    # 最坏情况: 只差 4 个字符(真实屏常见: GA201 -> GA202 只跳一位)
    w = ra.shape[1]
    seg = ra[:, int(w * 0.30):int(w * 0.62)]        # 取号码段所在横条
    seg_b = rb[:, int(w * 0.30):int(w * 0.62)]
    r_seg = roi_ratio(seg, seg_b)
    print("号码段(约占文字宽1/3) = %.4f  %s"
          % (r_seg, "安全(余量 %.1fx)" % (r_seg / THR) if r_seg > THR else "会漏检!"))
else:
    print("未在测试卡上找到文字区域, 跳过")


# ---------------------------------------------------------------- 亮度补偿验证
print()
print("=" * 76)
print("阈值取值验证: announce.py 新增「亮度漂移补偿」后, 能否把「真换号」")
print("(约 1%% 量级) 与「自动曝光漂移」区分开。阈值 = %.3f" % THR)
print("=" * 76)


def detect(a, b, thr_abs=25, compensate=True):
    """复刻 announce.py 当前的检测算法(灰度->缩到min(宽,480)->高斯模糊->占比)。"""
    def prep(f):
        g = cv2.cvtColor(f, cv2.COLOR_BGR2GRAY)
        tw = min(g.shape[1], 480)
        g = cv2.resize(g, (tw, max(1, int(g.shape[0] * tw / g.shape[1]))))
        return cv2.GaussianBlur(g, (5, 5), 0)
    ga, gb = prep(a), prep(b)
    if compensate:
        d = float(gb.mean()) - float(ga.mean())
        ref = cv2.convertScaleAbs(ga, alpha=1.0, beta=d) if abs(d) >= 1.0 else ga
    else:
        ref = ga
    return float((cv2.absdiff(gb, ref) > thr_abs).mean())


if bb:
    a_crop, b_crop = A[y0:y1, x0:x1], B[y0:y1, x0:x1]
    drift15 = np.clip(a_crop.astype(np.float32) + 15, 0, 255).astype(np.uint8)
    drift40 = np.clip(a_crop.astype(np.float32) + 40, 0, 255).astype(np.uint8)
    noise = np.clip(a_crop.astype(np.float32)
                    + np.random.normal(0, 8, a_crop.shape), 0, 255).astype(np.uint8)
    CASES = [
        ("真换号(文字变化)", a_crop, b_crop, "触发"),
        ("轻微漂移 +15", a_crop, drift15, "静默"),
        # 强漂移经补偿后仍有 0.0057 残差(像素加 40 会饱和到 255, 抵消不干净),
        # 略高于阈值。这里判为"可容忍": 多跑一次 OCR 只是白费点 CPU,
        # 号码已去重不会重复播报; 而把阈值抬高去躲它, 反而会漏掉真换号。
        ("强曝光漂移 +40", a_crop, drift40, "容忍"),
        ("仅传感器噪声 sigma8", a_crop, noise, "静默"),
        ("完全静止(自己比自己)", a_crop, a_crop, "静默"),
    ]
    print("%-22s %-14s %-14s %s" % ("场景", "旧算法", "新算法", "结论"))
    for name, x, y, expect in CASES:
        raw = detect(x, y, compensate=False)
        new = detect(x, y, compensate=True)
        fired = new > THR
        got = "触发" if fired else "静默"
        ok = "✓" if (expect == "容忍" or got == expect) else "✗"
        print("%-22s %-14.4f %-14.4f %s%s (期望%s)"
              % (name, raw, new, got, ok, expect))
    print()
    print("判读: 「真换号」必须触发(否则漏播); 噪声/静止必须静默(否则空烧CPU);")
    print("      强漂移触发可容忍 —— 只会多跑一次 OCR, 号码已去重不会重复播报。")


