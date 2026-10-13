#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""恢复 detect 的兜底/活跃期/确认窗口为上游设计值, 并做爬坑加固。"""
import json, shutil, time

P = "/opt/announce-rs/config.json"
shutil.copy2(P, P + ".bak." + time.strftime("%Y%m%d-%H%M%S"))
d = json.load(open(P, encoding="utf-8"))
det = d.setdefault("detect", {})

want = {"fallback_ocr_sec": 3.0, "active_sec": 3.0, "confirm_ttl_sec": 4.0,
        "min_interval_sec": 0.5, "static_fb_sec": 10.0}
for k, v in want.items():
    old = det.get(k)
    det[k] = v
    print("%-18s %s -> %s" % (k, old, v))

json.dump(d, open(P, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("\n--- 写入后 ---")
print(json.dumps(json.load(open(P, encoding="utf-8"))["detect"], ensure_ascii=False, indent=2))
