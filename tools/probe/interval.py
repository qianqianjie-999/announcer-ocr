#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""查看当前抓拍/识别节奏相关配置与运行时实测"""
import json, re, subprocess

d = json.load(open("/opt/announce-rs/config.json", encoding="utf-8"))
print("=== 配置 ===")
print("fps                 =", d.get("fps"), " (取帧帧率)")
print("capture             =", d.get("capture_width"), "x", d.get("capture_height"))
print("roi                 =", d.get("roi"))
det = d.get("detect") or {}
for k in ("diff_threshold", "changed_ratio", "stable_frames",
          "confirm_ttl_sec", "active_sec", "fallback_ocr_sec"):
    print("%-20s= %s" % (k, det.get(k)))

print("\n=== 运行时实测(最近30条监测行) ===")
out = subprocess.run(["bash", "-lc",
                      "grep -a '监测' /var/log/announce-rs/service.log | tail -30"],
                     capture_output=True, text=True).stdout
print(out)
