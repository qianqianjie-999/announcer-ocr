#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""紧 ROI 基准：自动定位"请XX到XX号窗口"叫号行，裁紧后测 OCR 耗时。
这是现场真实的 ROI 形态（只框叫号行），用于给出可信的延迟数字。
"""
import re
import sys
import time

import cv2
from rapidocr_onnxruntime import RapidOCR

PAT = re.compile(r"到.{0,6}号窗口")


def bench(label, fn, n=5):
    fn()
    ts = []
    r = None
    for _ in range(n):
        t = time.perf_counter()
        r = fn()
        ts.append(time.perf_counter() - t)
    print(f"    {label:<34} 最快 {min(ts)*1000:6.0f} ms  平均 {sum(ts)/len(ts)*1000:6.0f} ms")
    return min(ts) * 1000, r


def main():
    img = cv2.imread(sys.argv[1])
    print("原图:", img.shape)
    eng = RapidOCR()

    t, res = bench("原图 det+rec", lambda: eng(img))
    boxes = res[0] or []
    call = [b for b in boxes if PAT.search(b[1].replace(" ", ""))]
    print("    命中叫号行:", [b[1] for b in call])
    if not call:
        raise SystemExit("未找到叫号行")

    x0 = int(min(min(p[0] for p in b[0]) for b in call))
    y0 = int(min(min(p[1] for p in b[0]) for b in call))
    x1 = int(max(max(p[0] for p in b[0]) for b in call))
    y1 = int(max(max(p[1] for p in b[0]) for b in call))
    pad = 12
    x0, y0 = max(0, x0 - pad), max(0, y0 - pad)
    x1 = min(img.shape[1], x1 + pad)
    y1 = min(img.shape[0], y1 + pad)
    crop = img[y0:y1, x0:x1]
    print(f"    紧ROI = [{x0},{y0},{x1-x0},{y1-y0}]  尺寸 {crop.shape[1]}x{crop.shape[0]}")

    tB, resB = bench("紧ROI det+rec（现场真实配置）", lambda: eng(crop))
    print("    识别:", [b[1] for b in (resB[0] or [])])

    tC, resC = bench("紧ROI 仅rec（关 det）", lambda: eng(crop, use_det=False, use_cls=False))
    nC = len(resC[0]) if resC and resC[0] else 0
    print(f"    识别 {nC} 段:", [b[1] for b in (resC[0] or [])][:4])

    # 降采样
    for f in (0.75, 0.5):
        s = cv2.resize(crop, (int(crop.shape[1] * f), int(crop.shape[0] * f)))
        tD, resD = bench(f"紧ROI 缩放 {f:.2f} ({s.shape[1]}x{s.shape[0]})", lambda s=s: eng(s))
        print("    识别:", [b[1] for b in (resD[0] or [])][:3])

    # 放大(如果原图小)
    s = cv2.resize(crop, (crop.shape[1] * 2, crop.shape[0] * 2))
    tE, resE = bench(f"紧ROI 放大 2x ({s.shape[1]}x{s.shape[0]})", lambda: eng(s))
    print("    识别:", [b[1] for b in (resE[0] or [])][:3])

    print("=" * 60)
    print(f"结论：现场配置下单次 OCR = {tB:.0f} ms（紧ROI det+rec）")
    for label, t1 in (("stable_frames=2, recheck=0.5", 2), ("stable_frames=3, recheck=0.7", 3)):
        rr = 0.5 if t1 == 2 else 0.7
        ms = 167 + tB * t1 + rr * 1000 * (t1 - 1)
        print(f"  {label:<32} 首见→开口 {ms:.0f} ms "
              f"（端到端 +0.3~0.8s = {ms/1000+0.3:.1f}~{ms/1000+0.8:.1f} s）")


if __name__ == "__main__":
    main()
