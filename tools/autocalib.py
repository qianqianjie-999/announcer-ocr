#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""autocalib.py — 无桌面环境自动定位识别区域(ROI)

为什么需要它：
    calibrate.py 要弹 GUI 窗口用鼠标框选，服务器/SSH 部署时没有桌面，用不了。
    本脚本直接连相机抓帧 -> 整图 OCR -> 找出"请XX到XX号窗口"文字 ->
    自动算出包围盒 -> 写入 config.json 的 roi 字段，全程无需图形界面。

用法:
    python autocalib.py                  # 连相机自动定位(推荐)
    python autocalib.py --save out.jpg   # 同时存一张带框的预览图便于核对
    python autocalib.py --image a.jpg    # 用现成照片试算(注意:成像比例需与相机一致)
    python autocalib.py --padding 30     # 包围盒向外扩的像素数(默认24)
"""
import argparse
import json
import os
import re
import sys
import time

import cv2

CFG = "config.json"


def open_camera(cfg):
    """按 config.camera_source 打开相机，返回 (cap, src, is_net)"""
    src = cfg.get("camera_source", 0)
    if isinstance(src, str) and src.isdigit():
        src = int(src)
    if src == "simulate":
        raise SystemExit("当前是 simulate 模拟模式，请先把 camera_source 改成 0(USB) 或 RTSP 地址")
    is_net = isinstance(src, str) and src.lower().startswith(("rtsp://", "http://", "https://"))
    if is_net:
        os.environ["OPENCV_FFMPEG_CAPTURE_OPTIONS"] = (
            "rtsp_transport;tcp|fflags;nobuffer|flags;low_delay|max_delay;300000"
        )
    cap = cv2.VideoCapture(src)
    if not cap.isOpened():
        raise SystemExit(f"打不开相机: {src}\n  USB 相机检查 /dev/video* 是否存在、用户是否在 video 组\n"
                         f"  RTSP 检查地址/账号/密码与网络连通性")
    if not is_net:
        cap.set(cv2.CAP_PROP_AUTO_EXPOSURE, 0.25)
        cap.set(cv2.CAP_PROP_AUTOFOCUS, 0)
        cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg.get("capture_width", 1920))
        cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg.get("capture_height", 1080))
    return cap, src, is_net


def grab_best(cap, n=8, warmup=1.5):
    """预热后抓 n 帧，返回最清晰的一帧(拉普拉斯方差最大)"""
    time.sleep(warmup)
    best, best_score = None, -1
    for _ in range(n):
        ok, f = cap.read()
        if not ok or f is None:
            time.sleep(0.2)
            continue
        g = cv2.cvtColor(f, cv2.COLOR_BGR2GRAY)
        score = cv2.Laplacian(g, cv2.CV_64F).var()
        if score > best_score:
            best, best_score = f, score
        time.sleep(0.15)
    if best is None:
        raise SystemExit("取不到画面（相机被占用 / 拨杆遮挡 / 网络流异常）")
    print(f"取帧成功: {best.shape[1]}x{best.shape[0]}  清晰度={best_score:.0f}")
    return best


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--image", help="改用现成图片（不连相机）")
    ap.add_argument("--save", default="autocalib_preview.jpg", help="预览图输出路径")
    ap.add_argument("--padding", type=int, default=24, help="包围盒外扩像素")
    ap.add_argument("--write", action="store_true", help="直接写入 config.json（默认只预览）")
    args = ap.parse_args()

    cfg = json.load(open(CFG, encoding="utf-8"))
    pat = re.compile(cfg["ocr"]["pattern"])

    if args.image:
        frame = cv2.imread(args.image)
        if frame is None:
            raise SystemExit(f"读不到图片: {args.image}")
        print(f"使用图片: {args.image}  {frame.shape[1]}x{frame.shape[0]}")
        print("⚠️  注意：用照片算出的坐标只在'相机拍出同样构图'时可用；")
        print("    正式部署请直接连相机运行本脚本。")
    else:
        cap, src, is_net = open_camera(cfg)
        print(f"相机: {src}  类型: {'RTSP' if is_net else 'USB'}")
        frame = grab_best(cap)
        cap.release()
        # 存一张原帧，方便人工确认构图
        cv2.imwrite("autocalib_frame.jpg", frame)
        print("已保存原始帧: autocalib_frame.jpg")

    from rapidocr_onnxruntime import RapidOCR
    eng = RapidOCR()
    print("整图 OCR 中（弱机约 3~6 秒）...")
    t = time.time()
    result, _ = eng(frame)
    result = result or []
    print(f"OCR 完成 {time.time()-t:.1f}s，共 {len(result)} 段文本")

    def fscore(v):
        """rapidocr 不同版本 score 可能是 float 也可能是 str，统一成 float"""
        try:
            return float(v)
        except (TypeError, ValueError):
            return 0.0

    hits = []
    for box, text, score in [(r[0], r[1], r[2] if len(r) > 2 else 0.0) for r in result]:
        flat = text.replace(" ", "")
        if pat.search(flat):
            hits.append((box, text, fscore(score)))
    if not hits:
        print("\n未找到符合叫号格式的文本！")
        print("屏幕上现有文字：")
        for r in result:
            print("   ", r[1])
        raise SystemExit("\n请确认：①屏幕当前是否在显示叫号 ②config.json 的 ocr.pattern 是否匹配该屏格式")

    print("\n命中叫号行：")
    for _, text, sc in hits:
        print(f"   [{sc:.2f}] {text}")

    xs0 = int(min(min(p[0] for p in b) for b, _, _ in hits))
    ys0 = int(min(min(p[1] for p in b) for b, _, _ in hits))
    xs1 = int(max(max(p[0] for p in b) for b, _, _ in hits))
    ys1 = int(max(max(p[1] for p in b) for b, _, _ in hits))
    pad = args.padding
    x = max(0, xs0 - pad)
    y = max(0, ys0 - pad)
    w = min(frame.shape[1] - x, (xs1 - xs0) + pad * 2)
    h = min(frame.shape[0] - y, (ys1 - ys0) + pad * 2)
    roi = [int(x), int(y), int(w), int(h)]
    print(f"\n建议 ROI = {roi}   尺寸 {w}x{h}   占全屏 {(w*h)/(frame.shape[0]*frame.shape[1])*100:.1f}%")

    vis = frame.copy()
    cv2.rectangle(vis, (x, y), (x + w, y + h), (0, 0, 255), 4)
    cv2.putText(vis, f"ROI {roi}", (x, max(30, y - 12)),
                cv2.FONT_HERSHEY_SIMPLEX, 1.0, (0, 0, 255), 3)
    cv2.imwrite(args.save, vis)
    print(f"预览图已保存: {args.save}（可下载查看框是否套准）")

    # 用新 ROI 复测一次，确认识别依然正确
    print("\n用该 ROI 复测...")
    crop = frame[y:y + h, x:x + w]
    t = time.time()
    r2, _ = eng(crop)
    dt_ms = (time.time() - t) * 1000
    texts2 = [r[1] for r in (r2 or [])]
    ok2 = [t for t in texts2 if pat.search(t.replace(" ", ""))]
    print(f"   耗时 {dt_ms:.0f} ms   识别: {texts2}")
    print(f"   命中: {ok2}  -> {'✅ 通过' if ok2 else '❌ 该ROI识别不到，请调大 --padding'}")

    if args.write:
        cfg["roi"] = roi
        json.dump(cfg, open(CFG, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
        print(f"\n已写入 {CFG}: roi = {roi}")
    else:
        print(f"\n（预览模式，未写入。确认无误后加 --write 正式写入）")


if __name__ == "__main__":
    main()
