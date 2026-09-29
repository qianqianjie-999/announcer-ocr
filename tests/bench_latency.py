# -*- coding: utf-8 -*-
"""bench_latency.py — 实测各环节耗时: OCR推理 / 帧差检测 / TTS播报时长
用途: 给出端到端延迟的硬数据(不靠估算)
用法: python bench_latency.py <叫号屏照片路径>
"""
import os
import sys
import time

import cv2

PHOTO = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "assets", "sim_cardA.jpg")


def bench(fn, n=3, label=""):
    fn()  # 预热(首次含模型加载)
    ts = []
    for _ in range(n):
        t = time.time()
        fn()
        ts.append((time.time() - t) * 1000)
    print(f"{label}: 平均 {sum(ts)/len(ts):.0f} ms  (明细 {[f'{x:.0f}' for x in ts]})")
    return sum(ts) / len(ts)


img = cv2.imread(PHOTO)
print(f"测试图: {PHOTO} 尺寸={img.shape[1]}x{img.shape[0]}\n")

# ---------- 1. OCR 推理耗时 ----------
print("=" * 58)
print("【1】OCR 推理耗时 (RapidOCR, CPU)")
print("=" * 58)
from rapidocr_onnxruntime import RapidOCR
ocr_engine = RapidOCR()


def ocr_full():
    return ocr_engine(img)


def ocr_roi():
    x, y, w, h = 100, 180, 1400, 650
    return ocr_engine(img[y:y + h, x:x + w])


def ocr_roi_half():
    x, y, w, h = 100, 180, 1400, 650
    roi = img[y:y + h, x:x + w]
    small = cv2.resize(roi, (w // 2, h // 2))
    return ocr_engine(small)


t_full = bench(ocr_full, 3, "整帧(不裁剪)")
t_roi = bench(ocr_roi, 3, "裁剪ROI 1400x650")
t_half = bench(ocr_roi_half, 3, "ROI 降采样 700x325")
print(f"\n→ 裁剪ROI可省: {t_full - t_roi:.0f} ms; 再降采样可再省: {t_roi - t_half:.0f} ms")

# ---------- 2. 帧差检测耗时 ----------
print()
print("=" * 58)
print("【2】帧差变化检测耗时 (灰度+缩放+高斯模糊+absdiff)")
print("=" * 58)
x, y, w, h = 100, 180, 1400, 650
prev = None


def frame_diff():
    global prev
    roi = img[y:y + h, x:x + w]
    gray = cv2.cvtColor(roi, cv2.COLOR_BGR2GRAY)
    gray = cv2.resize(gray, (480, max(1, int(h * 480 / w))))
    gray = cv2.GaussianBlur(gray, (5, 5), 0)
    if prev is not None and prev.shape == gray.shape:
        diff = cv2.absdiff(gray, prev)
        _ = (diff > 25).mean()
    prev = gray


bench(frame_diff, 5, "单次帧差检测")

# ---------- 3. TTS 播报时长 ----------
print()
print("=" * 58)
print("【3】TTS 播报时长 (Windows SAPI, rate=135)")
print("=" * 58)
TEXT = "请GA999到C01号窗口办理"
try:
    import pythoncom
    import win32com.client
    pythoncom.CoInitialize()
    voice = win32com.client.Dispatch("SAPI.SpVoice")
    voice.Rate = max(-10, min(10, int((135 - 180) / 25)))
    t = time.time()
    voice.Speak(TEXT)
    one = (time.time() - t) * 1000
    print(f"单遍播报「{TEXT}」: {one:.0f} ms")
    t = time.time()
    voice.Speak(TEXT)
    voice.Speak(TEXT)
    two = (time.time() - t) * 1000
    print(f"连播2遍(程序实际行为): {two:.0f} ms")
except Exception as e:
    print(f"SAPI 不可用: {e}")
