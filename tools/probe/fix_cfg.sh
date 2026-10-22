#!/bin/bash
# 调整播报语速: piper_length_scale 越大越慢。1.1 -> 1.25 (约慢 14%)
set -e
P=/opt/announce-rs/config.json
cp -a "$P" "$P.bak.$(date +%Y%m%d-%H%M%S)"
# ★2026-10-23 低危修复: 原子写 —— 旧版 json.dump 直接写目标文件,
#   写盘中断留半截 JSON, announce-rs 的 serde_json 拒读崩循环
python3 - <<'PY'
import json, os
p = "/opt/announce-rs/config.json"
tmp = p + ".tmp"
d = json.load(open(p, encoding="utf-8"))
old = d["tts"].get("piper_length_scale")
d["tts"]["piper_length_scale"] = 1.25
json.dump(d, open(tmp, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
os.replace(tmp, p)
print("piper_length_scale: %s -> %s" % (old, d["tts"]["piper_length_scale"]))
PY
ls -l "$P"
