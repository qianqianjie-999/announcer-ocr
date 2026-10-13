#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""验证 config_web.validate_and_build 对 settle_sec 的兜底钳位。"""
import importlib.util
import json

spec = importlib.util.spec_from_file_location("cw", "/opt/announce-rs/config_web.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

cur = {
    "detect": {"diff_threshold": 25.0, "changed_ratio": 0.0015, "stable_frames": 2,
               "min_interval_sec": 0.5, "settle_sec": 0.5},
    "tts": {}, "ocr": {},
}


def build(settle):
    det = {"diff_threshold": 25.0, "changed_ratio": 0.0015, "stable_frames": 2,
           "confirm_ttl_sec": 4.0, "active_sec": 3.0, "fallback_ocr_sec": 3.0}
    if settle is not None:
        det["settle_sec"] = settle
    body = {
        "detect": det,
        "tts": {"enabled": True, "repeat": 3, "rate": 50},
        "ocr": {"lang": "chi_sim", "psm": 11},
        "src_type": "rtsp", "usb_idx": "", "rtsp": "rtsp://x",
        "capture_width": 704, "capture_height": 576, "fps": 3,
        "roi": [367, 187, 259, 64], "schedule": {"enabled": False},
    }
    cfg, err = m.validate_and_build(body)
    return err, (cfg["detect"].get("settle_sec") if cfg else None)


for label, val in [("传 0(会被清零)", 0), ("传空串('')", ""), ("不传该字段", None),
                   ("传 5(超上限)", 5), ("传 0.8(正常)", 0.8)]:
    err, out = build(val)
    print("  %-16s -> settle_sec = %s   err=%s" % (label, out, err))
