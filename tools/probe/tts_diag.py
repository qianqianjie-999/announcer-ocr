#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""诊断 piper 中文模型对拉丁字母的发音: 音素表 + 合成能量对比。"""
import json
import os
import struct
import subprocess
import wave

PIPER_DIR = "/opt/piper/piper"
MODEL = "/opt/piper/piper/zh_CN-huayan-medium.onnx"
JSON = MODEL + ".json"

print("=" * 60)
print("1) 音素表检查")
print("=" * 60)
d = json.load(open(JSON, encoding="utf-8"))
m = d.get("phoneme_id_map", {})
ks = list(m.keys())
print("音素总数:", len(ks))
print("是否有 (en) 标记 :", "(en)" in m)
print("是否有 (cmn) 标记:", "(cmn)" in m)
# 英语音素常用符号
eng = ["dZ", "eI", "s", "i", ":", "tS", "aU", "ou", "r", "N", "l", "@"]
print("英语相关音素存在情况:")
for k in eng:
    print("   %-5s %s" % (k, "有" if k in m else "★缺失"))
print()
print("全表(前 120 个):")
print(" ".join(ks[:120]))
print()
print("inference:", json.dumps(d.get("inference"), ensure_ascii=False))


def synth(text, out):
    env = dict(os.environ)
    env["LD_LIBRARY_PATH"] = PIPER_DIR + ":" + env.get("LD_LIBRARY_PATH", "")
    p = subprocess.run(
        [os.path.join(PIPER_DIR, "piper"), "-m", MODEL, "-f", out,
         "--length-scale", "1.1"],
        input=text.encode("utf-8"), stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE, env=env)
    if p.returncode != 0:
        return None
    return out


def rms(path):
    """返回 (时长s, 全局RMS, 峰值, 有声段占比)"""
    w = wave.open(path)
    n = w.getnframes()
    sr = w.getframerate()
    ch = w.getnchannels()
    sw = w.getsampwidth()
    raw = w.readframes(n)
    w.close()
    if sw != 2:
        return (n / sr, 0, 0, 0)
    cnt = len(raw) // 2
    vals = struct.unpack("<%dh" % cnt, raw[:cnt * 2])
    if ch == 2:
        vals = vals[::2]
    if not vals:
        return (0, 0, 0, 0)
    sq = sum(v * v for v in vals) / len(vals)
    peak = max(abs(v) for v in vals)
    # 有声帧占比: 以窗口 20ms 计算, 阈值取峰值 8%
    win = max(1, int(sr * 0.02))
    thr = peak * 0.08
    voiced = 0
    tot = 0
    for i in range(0, len(vals) - win, win):
        tot += 1
        seg = vals[i:i + win]
        if max(abs(v) for v in seg) > thr:
            voiced += 1
    return (n / sr, sq ** 0.5, peak, voiced / tot if tot else 0)


print()
print("=" * 60)
print("2) 合成能量对比 (时长 / RMS / 峰值 / 有声占比)")
print("=" * 60)
cases = [
    ("C", "C"),
    ("G", "G"),
    ("A", "A"),
    ("零", "零"),
    ("六", "六"),
    ("西", "西"),
    ("GA", "G A"),
    ("full_current", "请G A 一 六 二到C 零 六号窗口办理"),
    ("full_xi", "请G A 一 六 二到西 零 六号窗口办理"),
]
res = {}
for name, text in cases:
    out = "/tmp/diag_%s.wav" % name
    if synth(text, out) and os.path.exists(out):
        dur, r, pk, v = rms(out)
        res[name] = (dur, r, pk, v)
        print("%-14s 文本=%-28s 时长=%.3fs RMS=%7.1f 峰值=%6d 有声=%.0f%%"
              % (name, text, dur, r, pk, v * 100))
    else:
        print("%-14s 合成失败" % name)
    if os.path.exists(out):
        os.remove(out)
