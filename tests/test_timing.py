# -*- coding: utf-8 -*-
"""test_timing.py — 临时关掉TTS发声, 跑一小段模拟链路验证 [时序] 输出, 跑完恢复配置"""
import json
import os
import shutil
import subprocess
import sys
import time

PY = sys.executable
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")
CFG = os.path.join(ROOT, "config.json")
raw = open(CFG, encoding="utf-8").read()
cfg = json.loads(raw)

try:
    cfg["tts"]["enabled"] = False          # 关发声, 只测时序
    json.dump(cfg, open(CFG, "w", encoding="utf-8"),
              ensure_ascii=False, indent=2)

    shutil.copy(os.path.join(ASSETS, "test_card_B.jpg"), "sim_frame.jpg")
    p = subprocess.Popen([PY, "-u", os.path.join(ROOT, "announce.py")],
                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                         text=True, encoding="utf-8", errors="replace")
    time.sleep(5)
    shutil.copy(os.path.join(ASSETS, "test_card.jpg"), "sim_frame.jpg")
    time.sleep(7)
    p.terminate()
    out, _ = p.communicate(timeout=15)
    print(out)
finally:
    open(CFG, "w", encoding="utf-8").write(raw)
    print("--- config.json 已恢复 ---")
