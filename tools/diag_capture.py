# -*- coding: utf-8 -*-
"""diag_capture.py — 抓帧诊断: 直接看摄像头"到底拍到了什么"

用途: 当 announce.py 出现「心跳有变化但 OCR 读不出」时, 用它判断是
      【成像问题】(糊/太暗/太远/对焦跑偏) 还是【软件问题】(OCR/正则)。

用法: 先停掉 announce.py(USB摄像头独占, 两个进程同时开会把画面搞坏), 然后
         python diag_capture.py [总秒数] [每段秒数]
      默认 45 秒, 每 9 秒出一帧。

产出: _diag/frame_N.jpg  —— 每段里最清晰的一帧(可直接肉眼查看)
      控制台打印每段的 清晰度/亮度/OCR原文, 以及多种预处理对比。

注意: 必须用 imencode/imdecode 读写, Windows 下 OpenCV 不支持中文路径。
"""
import json
import os
import sys
import time

import cv2
import numpy as np

OUT_DIR = "_diag"


def imwrite_u(path, img):
    """中文路径安全的写图。"""
    ok, buf = cv2.imencode(".jpg", img, [int(cv2.IMWRITE_JPEG_QUALITY), 92])
    if not ok:
        return False
    with open(path, "wb") as f:
        f.write(buf.tobytes())
    return True


def sharpness(img):
    """Laplacian 方差: 越大越清晰。稳定合焦时通常 >50, 严重失焦 <20。"""
    g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    return float(cv2.Laplacian(g, cv2.CV_64F).var())


def main():
    total = float(sys.argv[1]) if len(sys.argv) > 1 else 45.0
    seg = float(sys.argv[2]) if len(sys.argv) > 2 else 9.0

    cfg = json.load(open("config.json", encoding="utf-8"))
    src = cfg.get("camera_source", 0)
    os.makedirs(OUT_DIR, exist_ok=True)

    # —— 与 announce.py 完全一致的相机打开方式 ——
    cap = cv2.VideoCapture(src)
    if not cap.isOpened():
        raise SystemExit("打不开摄像头(是否 announce.py 还在跑? 先停掉它)")
    cap.set(cv2.CAP_PROP_AUTO_EXPOSURE, 0.25)
    cap.set(cv2.CAP_PROP_AUTOFOCUS, 0)
    focus_req = cfg.get("camera_focus")
    if focus_req is not None:
        cap.set(cv2.CAP_PROP_FOCUS, int(focus_req))
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg.get("capture_width", 1920))
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg.get("capture_height", 1080))

    print("[信息] 请求 focus=%s, 实际读回 focus=%.0f autofocus=%.0f"
          % (focus_req, cap.get(cv2.CAP_PROP_FOCUS),
             cap.get(cv2.CAP_PROP_AUTOFOCUS)), flush=True)
    print("[信息] 实际分辨率 = %.0fx%.0f"
          % (cap.get(cv2.CAP_PROP_FRAME_WIDTH),
             cap.get(cv2.CAP_PROP_FRAME_HEIGHT)), flush=True)

    # —— OCR: 复用 announce.py 的引擎选择, 保证与生产完全一致 ——
    import announce
    ocr = announce.make_ocr()
    print("[信息] OCR 引擎就绪, 开始抓帧 %g 秒 —— 请把测试卡举到镜头前" % total,
          flush=True)

    t0 = time.time()
    n_seg = 0
    while time.time() - t0 < total:
        seg_end = min(time.time() + seg, t0 + total)
        best, best_sh, best_meta = None, -1.0, None
        n = 0
        while time.time() < seg_end:
            ok, frame = cap.read()
            if not ok:
                time.sleep(0.05)
                continue
            n += 1
            sh = sharpness(frame)
            mean = float(cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY).mean())
            if sh > best_sh:
                best, best_sh, best_meta = frame.copy(), sh, (mean, n)
            time.sleep(1.0 / 3)          # 与生产同样的 3fps 采样节奏

        if best is None:
            n_seg += 1
            continue

        mean, _ = best_meta
        tag = "清晰" if best_sh >= 50 else ("偏糊" if best_sh >= 20 else "很糊")
        path = os.path.join(OUT_DIR, "frame_%02d.jpg" % n_seg)
        imwrite_u(path, best)

        print("\n===== 第 %d 段 (采 %d 帧) =====" % (n_seg + 1, n), flush=True)
        print("  最清晰帧: 清晰度=%.1f (%s)  亮度=%.1f  已存 %s"
              % (best_sh, tag, mean, path), flush=True)

        # 变体A = 生产完全一致(整帧喂 OCR)。这是判断"生产为什么读不出"的基准。
        raw_a = ocr(best)
        print("  [整帧]     原文 = %r" % raw_a, flush=True)

        # 变体B = 中心裁切 + 1.5 倍放大。现场部署时 ROI 会紧贴屏幕, 不是整帧;
        #         这里模拟"框紧一点"能不能救回来。
        h, w = best.shape[:2]
        cx0, cy0 = int(w * 0.18), int(h * 0.18)
        crop = best[cy0:h - cy0, cx0:w - cx0]
        crop = cv2.resize(crop, None, fx=1.5, fy=1.5, interpolation=cv2.INTER_CUBIC)
        raw_b = ocr(crop)
        print("  [中心裁切] 原文 = %r   (清晰度 %.1f -> %.1f)"
              % (raw_b, best_sh, sharpness(crop)), flush=True)

        n_seg += 1

    cap.release()
    print("\n[完成] 共抓 %d 段, 图片在 %s/" % (n_seg, OUT_DIR), flush=True)


if __name__ == "__main__":
    main()
