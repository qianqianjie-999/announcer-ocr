#!/bin/bash
# 调整播报语速: piper_length_scale 越大越慢。1.1 -> 1.25 (约慢 14%)
set -e
P=/opt/announce-rs/config.json
cp -a "$P" "$P.bak.$(date +%Y%m%d-%H%M%S)"
python3 - <<'PY'
import json
p = "/opt/announce-rs/config.json"
d = json.load(open(p, encoding="utf-8"))
old = d["tts"].get("piper_length_scale")
d["tts"]["piper_length_scale"] = 1.25
json.dump(d, open(p, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("piper_length_scale: %s -> %s" % (old, d["tts"]["piper_length_scale"]))
PY
ls -l "$P"
