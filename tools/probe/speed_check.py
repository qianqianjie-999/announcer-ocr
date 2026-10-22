#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""语速实测 + 页面校验 + 最新播报"""
import json, re, subprocess, urllib.request, wave, os

# 1) 页面新版字段
try:
    h = urllib.request.urlopen("http://127.0.0.1:8080/", timeout=10).read().decode()
    print("页面长度 =", len(h))
    print("含 t_len 输入框 =", "t_len" in h)
    print("含 espeak 提示  =", "仅 espeak 引擎生效" in h)
except Exception as e:
    print("页面读取失败:", e)

# 2) 配置
d = json.load(open("/opt/announce-rs/config.json", encoding="utf-8"))
tts = d["tts"]
print("\n当前 piper_length_scale =", tts.get("piper_length_scale"))
print("ROI =", d.get("roi"), " capture =", d.get("capture_width"), d.get("capture_height"))

# 3) 实测合成时长（1.25 旧 vs 1.45 新）
TXT = "请 西 零 八 到 西 零 六 号 窗 口 办 理"
BIN = tts["piper_bin"]; MODEL = tts["piper_model"]; LIB = tts.get("piper_lib_dir") or ""
print("\n=== 合成时长对比（文本: %s）===" % TXT)
for ls in ("1.25", "1.45", "1.60"):
    out = "/tmp/_spd_%s.wav" % ls
    env = dict(os.environ); env["LD_LIBRARY_PATH"] = LIB
    p = subprocess.run([BIN, "--model", MODEL, "--output_file", out,
                        "--length-scale", ls],
                       input=TXT.encode(), capture_output=True, timeout=180, env=env)
    if p.returncode != 0:
        # ★2026-10-23 低危修复: 失败归因要带当前档位 —— 旧版硬编码 "1.45",
        #   任一档失败都被归到 1.45, 语速诊断结论指错档位
        print("  %s -> 合成失败:" % ls, p.stderr.decode()[:120]); continue
    w = wave.open(out); dur = w.getnframes() / float(w.getframerate()); w.close()
    print("  length-scale=%s -> 时长 %.2f 秒" % (ls, dur))
    os.remove(out)
