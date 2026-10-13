#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""用拍到的存证图走一遍完整链路: OCR -> 解析 -> 拼文本 -> piper 合成 -> aplay 播 1 遍。"""
import json, re, subprocess, sys, wave, os

CFG = json.load(open("/opt/announce-rs/config.json"))
IMG = sys.argv[1] if len(sys.argv) > 1 else "/opt/announce-rs/_live/trig_04.jpg"
print("输入图:", IMG, os.path.getsize(IMG), "bytes")

# ---- 1) OCR（与主程序同参数）----
ocr = CFG["ocr"]
cmd = [ocr.get("bin", "tesseract"), IMG, "stdout", "-l", ocr.get("lang", "chi_sim"),
       "--psm", str(ocr.get("psm", 11)), "--dpi", str(ocr.get("dpi", 150))]
r = subprocess.run(cmd, capture_output=True, text=True)
raw = " ".join(r.stdout.split())
print("OCR 原文:", repr(raw) if raw else "(空)")

# ---- 2) 正则解析 ----
num = win = None
for pat in ocr.get("pattern", []):
    m = re.search(pat, raw)
    if m:
        num, win = m.group(1), m.group(2)
        break
print("解析: number=%r window=%r" % (num, win))

# ---- 3) 拼播报文本（与主程序当前配置一致: 号码念字母, 窗口不念字母）----
tts = CFG["tts"]
DIG = "零一二三四五六七八九"

def spell_digits(s):          # 只念数字（窗口, speak_window_letter=False）
    return " ".join(DIG[int(c)] for c in s if c.isdigit())

def spell_tokens(s):          # 字母按中文谐音、数字转中文（号码, spell_number=True）
    L = {"A": "A", "B": "B", "C": "西", "D": "D", "E": "E", "F": "F", "G": "G"}
    return " ".join(DIG[int(c)] if c.isdigit() else L.get(c.upper(), c)
                    for c in s if not c.isspace())

if num is None:
    print("!! 图里认不出完整叫号（号码+窗口），无法播报")
    sys.exit(2)

n = spell_tokens(num) if tts.get("spell_number") else num
w = spell_tokens(win) if (win and tts.get("speak_window_letter")) else (spell_digits(win) if win else "")
text_out = tts["template"].replace("{number}", n).replace("{window}", w)
print("播报文本:", text_out)

# ---- 4) piper 合成 ----
wav = "/tmp/sim_announce.wav"
env = dict(os.environ, LD_LIBRARY_PATH="/opt/piper/piper")
r2 = subprocess.run([tts["piper_bin"], "-m", tts["piper_model"], "-f", wav,
                     "--length-scale", str(tts.get("piper_length_scale", 1.25))],
                    input=text_out.encode(), capture_output=True, env=env)
if r2.returncode != 0 or not os.path.exists(wav):
    print("piper 失败:", r2.stderr.decode(errors="replace")[:300]); sys.exit(3)
wv = wave.open(wav)
print("音频时长: %.2fs" % (wv.getnframes() / wv.getframerate()))
wv.close()

# ---- 5) 播放 1 遍 ----
dev = CFG.get("aplay_device", "plughw:0,0")
r3 = subprocess.run(["aplay", "-D", dev, wav], capture_output=True, text=True)
print("aplay:", (r3.stdout + r3.stderr).strip().splitlines()[-1] if (r3.stdout + r3.stderr).strip() else "ok")
os.remove(wav)
print("DONE played=1")
