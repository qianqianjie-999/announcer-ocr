# -*- coding: utf-8 -*-
"""test_ocr.py — 离线验证识别链路：图片 -> OCR -> 正则解析
用法: python test_ocr.py <图片路径> [更多图片...]
不用摄像头即可验证 OCR 与格式解析是否正确，部署前先跑通这一步。
"""
import json
import re
import sys

import cv2

def make_ocr():
    try:
        from rapidocr_onnxruntime import RapidOCR
        engine = RapidOCR()
        return lambda img: " ".join(r[1] for r in (engine(img)[0] or []))
    except ImportError:
        pass
    from paddleocr import PaddleOCR
    engine = PaddleOCR(use_angle_cls=False, lang="ch", show_log=False)
    return lambda img: " ".join(l[1][0] for l in (engine.ocr(img, cls=False)[0] or []))

def main():
    cfg = json.load(open("config.json", encoding="utf-8"))
    pat = re.compile(cfg["ocr"]["pattern"])
    ocr = make_ocr()

    for path in sys.argv[1:]:
        img = cv2.imdecode(
            __import__("numpy").fromfile(path, dtype="uint8"),  # 支持中文路径
            cv2.IMREAD_COLOR)
        if img is None:
            print(f"[失败] 读不了图片: {path}")
            continue
        print(f"\n=== {path} ({img.shape[1]}x{img.shape[0]}) ===")
        raw = ocr(img)
        print(f"OCR 原文: {raw}")
        m = pat.search(raw.replace(" ", ""))
        if m:
            print(f"解析结果: 号码={m.group(1)}  窗口={m.group(2)}  -> 将播报『请{m.group(1)}到{m.group(2)}号窗口办理』")
        else:
            print("解析结果: 未匹配叫号格式（检查正则或图片清晰度）")

if __name__ == "__main__":
    main()
