#!/usr/bin/env python3
# -*- coding: utf-8 -*-
import glob, json, os

print("=== 当前 config.json 的 detect 全字段 ===")
d = json.load(open("/opt/announce-rs/config.json", encoding="utf-8"))
print(json.dumps(d.get("detect"), ensure_ascii=False, indent=2))
print("fps =", d.get("fps"))

print("\n=== 历次备份里的 detect 演变(按时间) ===")
for p in sorted(glob.glob("/opt/announce-rs/config.json.bak.*")):
    try:
        x = json.load(open(p, encoding="utf-8"))
        det = x.get("detect") or {}
        print(os.path.basename(p),
              "| fb=%s act=%s ttl=%s stable=%s" % (
                  det.get("fallback_ocr_sec"), det.get("active_sec"),
                  det.get("confirm_ttl_sec"), det.get("stable_frames")))
    except Exception as e:
        print(p, "读取失败", e)
