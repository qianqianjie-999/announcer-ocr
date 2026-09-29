#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
focus_scan.py —— 手动对焦扫描：从 0~255 扫对焦值，找最清晰的一档并锁定

用途：USB 摄像头自动对焦失效/对焦环停在错误位置时，扫描找最佳焦点。
输出：focus_scan_result.txt + focus_best.jpg（最清晰一帧）
"""
import cv2
import numpy as np

W, H = 1920, 1080


def sharpness(img):
    g = cv2.cvtColor(img, cv2.COLOR_BGR2GRAY)
    return cv2.Laplacian(g, cv2.CV_64F).var()


def main():
    cap = cv2.VideoCapture(0)
    cap.set(cv2.CAP_PROP_FRAME_WIDTH, W)
    cap.set(cv2.CAP_PROP_FRAME_HEIGHT, H)
    for _ in range(10):
        cap.read()

    # 关自动对焦
    cap.set(cv2.CAP_PROP_AUTOFOCUS, 0)
    time_sleep = 0.15

    results = []
    print("扫描对焦值 0~255（镜头前请保持有物体不动，约 20 秒）...")
    for focus in range(0, 256, 25):
        cap.set(cv2.CAP_PROP_FOCUS, focus)
        # 丢弃前几帧让对焦马达到位
        f = None
        for _ in range(6):
            ok, f = cap.read()
        if not ok or f is None:
            print(f"  focus={focus:3d}: 取帧失败")
            continue
        s = sharpness(f)
        results.append((focus, s, f.copy()))
        print(f"  focus={focus:3d}: 锐度 {s:7.1f}")

    if not results:
        raise SystemExit("无有效帧")

    # 判断对焦设置是否生效: 各档锐度是否有明显差异
    vals = [s for _, s, _ in results]
    spread = max(vals) - min(vals)
    best_focus, best_s, best_f = max(results, key=lambda r: r[1])
    cv2.imwrite("focus_best.jpg", best_f)
    print(f"\n最清晰一档: focus={best_focus}, 锐度 {best_s:.1f}  -> focus_best.jpg")
    print(f"各档锐度极差: {spread:.1f}", end="  ")
    if spread < 8:
        print("→ 对焦值设置【未生效】，这只头大概率是固定焦距，软件调不了")
    else:
        print("→ 对焦值设置【生效】，可以把 config 里锁定 focus=%d" % best_focus)
    # 也报告自动对焦模式的锐度
    cap.set(cv2.CAP_PROP_AUTOFOCUS, 1)
    for _ in range(8):
        ok, f = cap.read()
    if ok:
        print(f"自动对焦模式下锐度: {sharpness(f):.1f}")
        cv2.imwrite("focus_af.jpg", f)
    cap.release()


if __name__ == "__main__":
    import time
    main()
