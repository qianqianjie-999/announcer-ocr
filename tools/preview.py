#!/usr/bin/env python
# -*- coding: utf-8 -*-
"""
preview.py —— 摄像头实时预览 + 在线 OCR 调试窗口（现场对机位、测距离用）

特点：
  · 实时显示摄像头画面，叠加识别区域（黄框）
  · 后台线程持续跑 OCR，把识别到的文字画在画面上（绿框=命中叫号格式）
  · 左上角显示「清晰度」数值：对着屏幕调距离/角度，把它调高就稳了
  · 画面左上角显示实时 OCR 原文，一眼看出识别到啥

按键：
  Q / ESC  退出
  SPACE    立即重跑一次 OCR
  S        保存当前画面为 preview_shot.jpg
  R        重新加载 config.json（改了ROI/正则可热更新）
"""
import json
import os
import re
import threading
import time

import cv2
import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)                 # 项目根目录(config.json 所在地)
CFG = os.path.join(ROOT, "config.json")


def load_cfg():
    with open(CFG, encoding="utf-8") as f:
        return json.load(f)


cfg = load_cfg()
src = cfg.get("camera_source", 0)
if isinstance(src, str) and src.isdigit():
    src = int(src)

state = {"res": [], "cost": 0.0, "busy": False, "frame_id": 0, "shot": None, "n": 0}
lock = threading.Lock()
stop = threading.Event()


def ocr_loop():
    from rapidocr_onnxruntime import RapidOCR
    eng = RapidOCR()
    last_id = -1
    while not stop.is_set():
        with lock:
            fid = state["frame_id"]
            frame = state["shot"]
            roi = cfg["roi"]
        if frame is None or fid == last_id:
            time.sleep(0.05)
            continue
        last_id = fid
        x, y, w, h = roi
        x2, y2 = min(x + w, frame.shape[1]), min(y + h, frame.shape[0])
        crop = frame[y:y2, x:x2]
        state["busy"] = True
        t0 = time.time()
        try:
            res, _ = eng(crop)
        except Exception as e:
            print("[OCR错误]", e)
            res = []
        cost = (time.time() - t0) * 1000
        with lock:
            state["res"] = res or []
            state["cost"] = cost
            state["busy"] = False
            state["n"] += 1
        time.sleep(0.1)


def apply_camera_cfg(cap):
    """应用 config 中的分辨率/对焦设置（初次打开和按 R 热更新时都会调用）"""
    if isinstance(src, int):
        cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg.get("capture_width", 1920))
        cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg.get("capture_height", 1080))
        cap.set(cv2.CAP_PROP_BUFFERSIZE, 1)
        cap.set(cv2.CAP_PROP_AUTOFOCUS, 0)
        focus = cfg.get("camera_focus")
        if focus is not None:
            cap.set(cv2.CAP_PROP_FOCUS, int(focus))


def main():
    global cfg
    cap = cv2.VideoCapture(src)
    apply_camera_cfg(cap)
    if not cap.isOpened():
        raise SystemExit(f"摄像头打开失败: {src}")

    t = threading.Thread(target=ocr_loop, daemon=True)
    t.start()

    win = "preview  |  Q=quit  SPACE=OCR now  S=save  R=reload cfg"
    cv2.namedWindow(win, cv2.WINDOW_NORMAL)
    cv2.resizeWindow(win, 1280, 720)
    print("预览窗口已打开。调整手机/卡片位置，让清晰度尽可能高、文字框变绿。")

    fails = 0
    while True:
        ok, frame = cap.read()
        if not ok:
            fails += 1
            if fails > 30:
                print("[错误] 连续取帧失败，退出")
                break
            time.sleep(0.1)
            continue
        fails = 0

        roi = cfg["roi"]
        with lock:
            state["frame_id"] += 1
            state["shot"] = frame.copy()
            res = list(state["res"])
            cost = state["cost"]
            n = state["n"]
            busy = state["busy"]

        vis = frame.copy()
        x, y, w, h = roi
        cv2.rectangle(vis, (x, y), (x + w, y + h), (0, 215, 255), 2)

        # 清晰度（拉普拉斯方差）
        crop = frame[y:y + h, x:x + w]
        if crop.size:
            g = cv2.cvtColor(crop, cv2.COLOR_BGR2GRAY)
            sharp = cv2.Laplacian(g, cv2.CV_64F).var()
        else:
            sharp = 0
        color = (0, 220, 0) if sharp > 100 else ((0, 200, 255) if sharp > 40 else (0, 0, 255))
        cv2.putText(vis, f"sharpness {sharp:6.1f}  ({'GOOD' if sharp > 100 else 'LOW - move closer / refocus'})",
                    (12, 34), cv2.FONT_HERSHEY_SIMPLEX, 0.85, color, 2)

        pat = re.compile(cfg["ocr"]["pattern"])
        hits = []
        for r in res:
            box, text = r[0], str(r[1])
            flat = text.replace(" ", "")
            m = pat.search(flat)
            pts = np.array(box, dtype=np.int32).reshape(-1, 1, 2)
            if m:
                hits.append((m.group(1), m.group(2), text))
                cv2.polylines(vis, [pts], True, (0, 220, 0), 3)
            else:
                cv2.polylines(vis, [pts], True, (200, 200, 200), 1)

        cv2.putText(vis, f"ocr #{n}  {cost:.0f}ms  {len(res)} boxes  {len(hits)} HIT"
                    + ("  [running]" if busy else ""),
                    (12, 68), cv2.FONT_HERSHEY_SIMPLEX, 0.85, (255, 80, 80), 2)

        yy = 104
        for num, win_id, raw in hits[:4]:
            cv2.putText(vis, f"HIT: {num} -> {win_id}", (12, yy),
                        cv2.FONT_HERSHEY_SIMPLEX, 0.95, (0, 255, 0), 2)
            yy += 32
        if not hits and res:
            cv2.putText(vis, f"read {len(res)} text, no number matched", (12, yy),
                        cv2.FONT_HERSHEY_SIMPLEX, 0.8, (0, 200, 255), 2)

        cv2.imshow(win, vis)
        k = cv2.waitKey(1) & 0xFF
        if k in (ord("q"), 27):
            break
        elif k == ord("s"):
            cv2.imwrite("preview_shot.jpg", vis)
            print("已保存 preview_shot.jpg")
        elif k == ord("r"):
            cfg = load_cfg()
            apply_camera_cfg(cap)
            print("已重新加载 config.json（含对焦/分辨率）")

    stop.set()
    cap.release()
    cv2.destroyAllWindows()
    print("预览已关闭")


if __name__ == "__main__":
    main()
