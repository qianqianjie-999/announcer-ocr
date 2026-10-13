#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""端到端验证「号码解析」: 真实 tesseract OCR -> 复刻 Rust 的 parse() 归一化。

复刻要点(与 deploy-rs/src/ocr.rs 一致):
  1) 正则用 config.json 里的 ocr.pattern(已锚定 G 前缀);
  2) split_prefix: 取最多 2 个前导大写字母当前缀;
  3) only_digits: 数字段只留数字(丢掉夹进来的字母噪声);
  4) 窗口 norm + 前导零折叠。
"""
import json
import re
import subprocess
import sys
import os

CFG = json.load(open("/opt/announce-rs/config.json"))
IMG = sys.argv[1]
print("图形:", IMG, os.path.getsize(IMG), "bytes")

ocr = CFG["ocr"]
cmd = [ocr.get("bin", "tesseract"), IMG, "stdout", "-l", ocr.get("lang", "chi_sim"),
       "--psm", str(ocr.get("psm", 11)), "--dpi", str(ocr.get("dpi", 150))]
r = subprocess.run(cmd, capture_output=True, text=True)
raw = " ".join(r.stdout.split())
print("OCR 原文:", repr(raw) if raw else "(空)")

flat = "".join(c for c in raw if not c.isspace() and c != "?")


def split_prefix(s):
    i = 0
    while i < len(s) and i < 2 and s[i].isupper():
        i += 1
    return s[:i], s[i:]


def norm(s):
    return s.replace("O", "0").replace("I", "1").replace("l", "1")


def only_digits(s):
    return "".join(c for c in s if c.isdigit())


def fold_window_zeros(s):
    if not s or not s[0].isupper():
        return s
    letters = 0
    while letters < len(s) and s[letters].isupper():
        letters += 1
    rest = s[letters:]
    zeros = len(rest) - len(rest.lstrip("0"))
    if zeros >= 2:
        return s[:letters] + "0" + rest[zeros:]
    return s


out = set()
for pat in ocr.get("pattern", []):
    for m in re.finditer(pat, flat):
        pre, rest = split_prefix(m.group(1))
        digits = only_digits(norm(rest))
        if not digits:
            continue
        out.add((pre + digits, fold_window_zeros(norm(m.group(2)))))

print("解析候选:", sorted(out) if out else "(无)")
if out:
    for n, w in sorted(out):
        print("  -> 会播报: 请 %s 到 %s 号窗口办理" % (n, w))
