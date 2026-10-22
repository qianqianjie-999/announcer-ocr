#!/bin/bash
# 修 OCR 正则: 消除"请GA172到号窗口办理"半截话
# 根因(2026-10-13 存证截图实证):
#   ① 旧 pattern2 要求窗口号后有"号/窗"字, 而 ROI 右缘常把"号窗口办理"截掉,
#      只剩"请GA172到C04" -> pattern2 匹配不上;
#   ② 此时只剩 pattern1(仅号码)命中 -> 产生 (GA172,"") 空窗口候选 ->
#      模板套出"请GA172到号窗口办理", repeat=3 播三遍废话;
#      等某轮 OCR 认出完整窗口, (GA172,"C04") 是新键, 又播三遍正题。
# 修法:
#   - 删掉"仅号码"的 pattern1(现场叫号屏必有窗口, 它只会产出半截候选);
#   - pattern2 去掉尾部 [号窗] 硬性要求 -> "请GA172到C04" 也能出带窗口的候选。
#   pattern 页面不可编辑, Web 保存时自动沿用, 不会被覆盖。
set -e
P=/opt/announce-rs/config.json
cp -a "$P" "$P.bak.$(date +%Y%m%d-%H%M%S)"
python3 - <<'PY'
import json, os
p = "/opt/announce-rs/config.json"
d = json.load(open(p, encoding="utf-8"))
old = d["ocr"].get("pattern")
new = ["([A-Z]{1,2}\\d{3,6})[到至]([A-Z]{0,2}\\d{1,3})"]
d["ocr"]["pattern"] = new
json.dump(d, open(p + ".tmp", "w", encoding="utf-8"), ensure_ascii=False, indent=2)
os.replace(p + ".tmp", p)
print("旧 pattern:", old)
print("新 pattern:", new)
PY
