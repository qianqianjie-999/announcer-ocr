#!/usr/bin/env python3
"""把现场 config.json 的 ocr.pattern 锚定到 G 前缀, 并补 detect.settle_sec。

起因(2026-10-13): 原正则 `([A-Z]{1,2}\\d{3,6})[到至](...)` 在 "GAT174" 上会回溯切出
"AT174"(G 后多余字母被当成号码前缀) -> 与 GA174 是不同去重键 -> 同一个号播两遍。
锚定成 `G[A-Z]{0,2}\\d{2,6}` 后不再匹配到 AT174; 而 "GAT174" 仍能命中,
配合 ocr.rs 的 only_digits() 把数字段夹的字母丢掉 -> 还原成正确的 GA174。
"""
import json
import shutil
import time
from pathlib import Path

P = Path("/opt/announce-rs/config.json")
NEW_PAT = r"(G[A-Z]{0,2}\d{2,6})[到至]([A-Z]{0,2}\d{1,3})"

bak = P.with_name(P.name + ".bak." + time.strftime("%Y%m%d-%H%M%S"))
shutil.copy2(P, bak)
print("[备份]", bak)

d = json.loads(P.read_text(encoding="utf-8"))

ocr = d.setdefault("ocr", {})
print("[旧 pattern]", ocr.get("pattern"))
ocr["pattern"] = [NEW_PAT]
print("[新 pattern]", ocr["pattern"])

det = d.setdefault("detect", {})
det.setdefault("settle_sec", 0.5)
print("[detect]  settle_sec =", det["settle_sec"])

# ★2026-10-23 低危修复: 原子写 —— 旧版 write_text 直接写目标文件,
#   写盘中断留半截 JSON, announce-rs 的 serde_json 拒读崩循环
tmp = P.with_name(P.name + ".tmp")
tmp.write_text(json.dumps(d, ensure_ascii=False, indent=2), encoding="utf-8")
tmp.replace(P)
print("[写入完成]")

# 回读校验
d2 = json.loads(P.read_text(encoding="utf-8"))
print("[回读] ocr.pattern =", d2["ocr"]["pattern"])
print("[回读] detect     =", json.dumps(d2["detect"], ensure_ascii=False))
