# -*- coding: utf-8 -*-
"""test_rapid_switch.py — 模拟"单行叫号屏连续跳号", 验证每个号都播到、顺序正确
单行屏: 新号出现时旧号直接被替换(旧号会消失), 最容易漏播的场景。
"""
import json
import os
import shutil
import subprocess
import sys
import time

from PIL import Image, ImageDraw, ImageFont

PY = sys.executable
FONT = r"C:\Windows\Fonts\simhei.ttf"


def card(name, text):
    img = Image.new("RGB", (1600, 900), (18, 60, 130))
    d = ImageDraw.Draw(img)
    d.text((800, 200), "等待 2人", font=ImageFont.truetype(FONT, 56), fill="white", anchor="mm")
    d.text((800, 450), text, font=ImageFont.truetype(FONT, 110), fill="white", anchor="mm")
    img.save(name, quality=97)


card("sim1.jpg", "请GA999到C01号窗口办理")
card("sim2.jpg", "请GA777到C02号窗口办理")
card("sim3.jpg", "请GA555到C03号窗口办理")

raw = open("config.json", encoding="utf-8").read()
cfg = json.loads(raw)
try:
    cfg["tts"]["enabled"] = False              # 关发声, 只看识别与排队结果
    json.dump(cfg, open("config.json", "w", encoding="utf-8"),
              ensure_ascii=False, indent=2)
    if os.path.exists("announce_log.csv"):
        os.remove("announce_log.csv")

    shutil.copy("sim1.jpg", "sim_frame.jpg")
    p = subprocess.Popen([PY, "-u", "announce.py"], stdout=subprocess.PIPE,
                         stderr=subprocess.STDOUT, text=True,
                         encoding="utf-8", errors="replace")

    time.sleep(6)
    shutil.copy("sim2.jpg", "sim_frame.jpg")
    print(f"{time.strftime('%H:%M:%S')} >>> 屏幕跳号: GA999 → GA777 (旧号被替换)")
    time.sleep(2)
    shutil.copy("sim3.jpg", "sim_frame.jpg")
    print(f"{time.strftime('%H:%M:%S')} >>> 屏幕再跳号: GA777 → GA555 (仅隔2秒)")
    time.sleep(9)

    p.terminate()
    out, _ = p.communicate(timeout=15)
    print("--- 程序输出 ---")
    print(out)
    print("--- 识别日志 ---")
    print(open("announce_log.csv", encoding="utf-8-sig").read()
          if os.path.exists("announce_log.csv") else "(无)")
finally:
    open("config.json", "w", encoding="utf-8").write(raw)
    shutil.copy("sim1.jpg", "sim_frame.jpg")
    print("--- config.json 已恢复 ---")
