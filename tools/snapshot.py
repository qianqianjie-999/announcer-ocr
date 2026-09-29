#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
snapshot.py —— 现场诊断快照工具（识别不到号码时用这个查原因）

作用：从摄像头抓一帧 → 跑 OCR → 打印"OCR 到底看到了什么" → 标注匹配结果存图。
用途：程序一直记 INVALID 时，先跑它，就能分清是
      ① 摄像头没拍到屏幕 / 位置不对
      ② 字太小/太糊，OCR 读不出
      ③ 读出来了但格式与 config.ocr.pattern 不符

用法：
    python snapshot.py                 # 用 config.json 的 camera_source 抓一帧
    python snapshot.py --source 0      # 指定摄像头索引
    python snapshot.py --source "rtsp://..."
    python snapshot.py --raw           # 不做 ROI 裁剪, 整帧识别
    python snapshot.py --out snap.jpg  # 指定输出图（默认 snapshot.jpg）
"""
import argparse
import json
import os
import re
import sys
import time

import cv2

CFG = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                   "config.json")            # 项目根目录
if not os.path.exists(CFG):
    CFG = "config.json"                      # 兼容旧扁平布局(部署机上)


def load_cfg():
    with open(CFG, encoding="utf-8") as f:
        return json.load(f)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", default=None, help="摄像头索引或RTSP地址，默认取config")
    ap.add_argument("--out", default="snapshot.jpg")
    ap.add_argument("--raw", action="store_true", help="整帧识别，不做ROI裁剪")
    ap.add_argument("--frames", type=int, default=10, help="预热丢弃帧数")
    args = ap.parse_args()

    cfg = load_cfg()
    src = args.source if args.source is not None else cfg.get("camera_source", 0)
    if isinstance(src, str) and src.isdigit():
        src = int(src)

    print(f"[1/4] 连接取流源: {src}")
    cap = cv2.VideoCapture(src)
    if isinstance(src, int):
        cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg.get("capture_width", 1920))
        cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg.get("capture_height", 1080))
    if not cap.isOpened():
        raise SystemExit(f"摄像头打开失败: {src}")

    frame = None
    # 预热：等自动曝光稳定（首帧往往过暗/过曝，OCR 会误判）
    for _ in range(max(1, args.frames)):
        ok, f = cap.read()
        if ok:
            frame = f
        time.sleep(0.05)
    cap.release()
    if frame is None:
        raise SystemExit("取帧失败，检查摄像头是否被其他程序占用")

    h, w = frame.shape[:2]
    print(f"[2/4] 取帧成功: {w}x{h}  (已丢弃 {args.frames} 帧预热)")

    if args.raw:
        roi = (0, 0, w, h)
    else:
        roi = tuple(cfg["roi"])
        if roi[0] + roi[2] > w or roi[1] + roi[3] > h:
            print(f"      [警告] config.roi {list(roi)} 超出画面 {w}x{h}，已按整帧处理")
            roi = (0, 0, w, h)
    x, y, rw, rh = roi
    print(f"[3/4] 识别区域 ROI: x={x} y={y} w={rw} h={rh}  (占全图 {rw * rh / (w * h) * 100:.1f}%)")

    print("[4/4] 跑 OCR ...")
    from rapidocr_onnxruntime import RapidOCR
    import numpy as np
    ocr = RapidOCR()
    t0 = time.time()
    res, _ = ocr(frame[y:y + rh, x:x + rw])
    cost = (time.time() - t0) * 1000

    pat = re.compile(cfg["ocr"]["pattern"])
    vis = frame.copy()
    cv2.rectangle(vis, (x, y), (x + rw, y + rh), (0, 200, 255), 2)

    if not res:
        print(f"\n>>> OCR 未识别到任何文字（耗时 {cost:.0f} ms）")
        print("    原因基本是：镜头没对准屏幕 / 画面过暗过曝 / 距离太远字太小 / 对焦糊了")
    else:
        print(f"\n>>> OCR 共识别 {len(res)} 段文字（耗时 {cost:.0f} ms）：")
        hit = 0
        for box, text, score in [(r[0], r[1], r[2] if len(r) > 2 else 0) for r in res]:
            flat = str(text).replace(" ", "")
            m = pat.search(flat)
            if m:
                hit += 1
                print(f"    ★命中 [{score}] {text}   -> 号码={m.group(1)} 窗口={m.group(2)}")
                color = (0, 200, 0)
            else:
                print(f"     普通 [{score}] {text}")
                color = (180, 180, 180)
            pts = np.array(box, dtype=np.int32).reshape(-1, 1, 2)
            cv2.polylines(vis, [pts], True, color, 2)
        cv2.putText(vis, f"OCR {len(res)} boxes / {hit} hit / {cost:.0f}ms",
                    (10, 32), cv2.FONT_HERSHEY_SIMPLEX, 0.9, (0, 0, 255), 2)
        if hit == 0:
            print("\n    ⚠ 有文字但一条都不匹配 config.ocr.pattern —— 改正则，或屏幕格式与预期不同")
            print(f"    当前正则: {cfg['ocr']['pattern']}")

    cv2.imwrite(args.out, vis)
    print(f"\n标注图已保存: {args.out}（绿框=命中，灰框=其他文字，黄框=识别区域）")


if __name__ == "__main__":
    main()
