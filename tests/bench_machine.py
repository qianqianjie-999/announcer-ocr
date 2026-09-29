#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""机器标尺：在同一张叫号屏照片上测各环节耗时，输出端到端延迟推算。
Windows / Linux 通用，用于跨机对比。
    python bench_machine.py <叫号屏照片> [roi_x,roi_y,roi_w,roi_h]
"""
import os
import platform
import sys
import time

import cv2
import numpy as np
from rapidocr_onnxruntime import RapidOCR

# ---- 程序当前参数（与 config.json 保持一致） ----
FPS = 3.0
RECHECK_SEC = 0.5
CONFIRM_ROUNDS = 2          # stable_frames


def matmul_ms():
    a = np.random.rand(300, 300).astype("float32")
    b = a.copy()
    for _ in range(3):
        np.dot(a, b)
    ts = []
    for _ in range(10):
        t = time.perf_counter()
        np.dot(a, b)
        ts.append(time.perf_counter() - t)
    return min(ts) * 1000


def bench(label, fn, n=3):
    fn()                                    # 预热
    ts = []
    r = None
    for _ in range(n):
        t = time.perf_counter()
        r = fn()
        ts.append(time.perf_counter() - t)
    print(f"    {label:<28} 最快 {min(ts)*1000:7.0f} ms   平均 {sum(ts)/len(ts)*1000:7.0f} ms")
    return min(ts), r


def main():
    if len(sys.argv) < 2:
        raise SystemExit(__doc__)
    img_path = sys.argv[1]

    print("=" * 66)
    print(f"主机: {platform.node()}  |  {platform.system()} {platform.release()}")
    print(f"Python: {platform.python_version()}  |  CPU 逻辑核: {os.cpu_count()}")
    print(f"numpy {np.__version__} | opencv {cv2.__version__}")
    import onnxruntime as ort
    print(f"onnxruntime {ort.__version__} | {ort.get_available_providers()}")
    print(f"300x300 matmul 基准: {matmul_ms():.3f} ms")
    print("=" * 66)

    t0 = time.perf_counter()
    img = cv2.imread(img_path)
    if img is None:
        raise SystemExit(f"读不到图片: {img_path}")
    t_load = (time.perf_counter() - t0) * 1000
    print(f"图像: {img.shape[1]}x{img.shape[0]}  读取耗时 {t_load:.0f} ms")

    eng = RapidOCR()

    # A. 整图 det+rec
    tA, resA = bench("A. 整图 det+rec", lambda: eng(img))
    texts = [x[1] for x in (resA[0] or [])]
    print(f"       识别到 {len(texts)} 段: {texts[:6]}")

    # 从检测框推算"叫号行"包围盒，作为 ROI
    if resA[0]:
        xs0 = min(min(p[0] for p in b[0]) for b in resA[0])
        ys0 = min(min(p[1] for p in b[0]) for b in resA[0])
        xs1 = max(max(p[0] for p in b[0]) for b in resA[0])
        ys1 = max(max(p[1] for p in b[0]) for b in resA[0])
        x, y = max(0, int(xs0) - 10), max(0, int(ys0) - 10)
        w = min(img.shape[1] - x, int(xs1 - xs0) + 20)
        h = min(img.shape[0] - y, int(ys1 - ys0) + 20)
    else:
        x, y, w, h = 0, 0, img.shape[1], img.shape[0]
    if len(sys.argv) > 2:
        x, y, w, h = [int(v) for v in sys.argv[2].split(",")]
    roi = img[y:y + h, x:x + w]
    print(f"ROI = [{x},{y},{w},{h}]  尺寸 {roi.shape[1]}x{roi.shape[0]}")

    bench("B. ROI det+rec（现行）", lambda: eng(roi))
    bench("C. ROI 仅 rec（关 det）", lambda: eng(roi, use_det=False, use_cls=False))

    # 降采样
    small = cv2.resize(roi, (roi.shape[1] // 2, roi.shape[0] // 2))
    bench("D. ROI 降采样一半", lambda: eng(small))

    print("=" * 66)
    tB, resB = bench("B. ROI det+rec（复测）", lambda: eng(roi), n=5)
    ocr_ms = tB * 1000
    poll = 1000.0 / FPS
    open_ms = poll / 2 + ocr_ms * CONFIRM_ROUNDS + RECHECK_SEC * (CONFIRM_ROUNDS - 1) * 1000
    print(f"单次 OCR (ROI)                          : {ocr_ms:.0f} ms")
    print(f"等待轮询(平均)                          : {poll/2:.0f} ms")
    print(f"确认 {CONFIRM_ROUNDS} 轮 OCR + 间隔 {RECHECK_SEC}s          : "
          f"{ocr_ms*CONFIRM_ROUNDS + RECHECK_SEC*(CONFIRM_ROUNDS-1)*1000:.0f} ms")
    print(f"→ 首见→开始播报（纯计算，不含相机/网络）: {open_ms:.0f} ms")
    print(f"→ 预估端到端（+ 相机与网络 300~800ms）  : "
          f"{open_ms+300:.0f} ~ {open_ms+800:.0f} ms")
    print("=" * 66)
    if resB[0]:
        print("解析文本:", [x[1] for x in resB[0]])


if __name__ == "__main__":
    main()
