#!/bin/bash
# 调整 piper 语速(piper_length_scale) —— 1.0=常速, 越大越慢。需 root。
set -e
CFG=/opt/announce-rs/config.json
NEW="${1:-1.45}"

cp -a "$CFG" "$CFG.bak.$(date +%Y%m%d-%H%M%S)"
echo "备份完成:"; ls -1t "$CFG".bak.* 2>/dev/null | head -3

python3 - "$CFG" "$NEW" <<'PY'
import json, sys
p, new = sys.argv[1], float(sys.argv[2])
d = json.load(open(p, encoding="utf-8"))
tts = d.setdefault("tts", {})
old = tts.get("piper_length_scale")
tts["piper_length_scale"] = new
json.dump(d, open(p, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("语速 piper_length_scale: %s -> %s" % (old, new))
PY

echo "--- 复核 ---"
python3 -c "import json;d=json.load(open('$CFG'));print(json.dumps(d['tts'],ensure_ascii=False,indent=2))"
