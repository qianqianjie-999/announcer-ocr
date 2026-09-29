# -*- coding: utf-8 -*-
"""拿历史日志里的真实 OCR 文本回归测试新正则: 该救回的救回, 该拒绝的拒绝。"""
import csv
import json
import re

cfg = json.load(open("config.json", encoding="utf-8"))
pats = cfg["ocr"]["pattern"]
if isinstance(pats, str):
    pats = [pats]
pats = [re.compile(p) for p in pats]


def norm(s):
    return s.replace("O", "0").replace("I", "1").replace("l", "1")


def parse(txt):
    out = set()
    flat = txt.replace(" ", "")
    for p in pats:
        for m in p.finditer(flat):
            num = m.group(1)
            m2 = re.match(r"([A-Z]{1,2})(.*)", num)
            if m2:
                num = m2.group(1) + norm(m2.group(2))
            out.add((num, norm(m.group(2))))
    return out


# 1) 关键用例(手工标注期望值)
CASES = [
    # (OCR原文, 期望解析出的号, 说明)
    ("等待2人 请GA999到C01国口办理 请GA200至 8号窗口办理", {"GA999/C01", "GA200/8"}, "国=窗错读 + 至=到错读 + C08丢字母C(三错并发)"),
    ("等待2人 青GA777到C03号窗办理 青GA999到C01号窗口办理", {"GA777/C03", "GA999/C01"}, "青=请错读(旧版能过)"),
    ("请GA200到CO", set(), "文本截断, 无号/窗/口 -> 必须拒绝"),
    ("等特2人 请GA777到C03号 口办 请GA999到Q", {"GA777/C03"}, "缺窗字(旧版整条漏播)"),
    ("等持2人 办理 请GA777到C03 窗口办理 请GA999", {"GA777/C03"}, "缺号字(旧版整条漏播)"),
    ("请GA999到C01号窗口办理 请GA200到C08号窗口办理", {"GA999/C01", "GA200/C08"}, "标准清读"),
    ("103110", set(), "纯数字噪声"),
    ("COLLECT", set(), "英文噪声"),
    ("1O31109", set(), "数字噪声"),
    ("等待2人 请G99到C01号口办理 请GA200到 号窗口办理", set(), "号码位数不足 + 窗口全丢 -> 拒绝"),
    ("M到C03号窗口办理 A999到C01号口办理", {"A999/C01"}, "GA被读成A(前缀丢失)"),
]

print("=" * 78)
print("【关键用例】")
print("=" * 78)
bad = 0
for raw, want, note in CASES:
    got = {"%s/%s" % t for t in parse(raw)}
    ok = (got == want)
    bar = "PASS" if ok else "FAIL"
    if not ok:
        bad += 1
    print("%-4s %-30s 期望=%-22s 实得=%-22s" % (bar, note, want or "{}", got or "{}"))

# 2) 全量日志回放: 统计新正则把多少 INVALID 救成可播
print()
print("=" * 78)
print("【历史日志全量回放】")
print("=" * 78)
import glob
for path in ["announce_log_旧_1447.csv", "announce_log_1454成功3条.csv"]:
    old_ok = new_ok = inv = rescued = 0
    try:
        rows = list(csv.reader(open(path, encoding="utf-8-sig")))
    except FileNotFoundError:
        continue
    rescued_samples = []
    for r in rows:
        if len(r) < 5:
            continue
        raw, status = r[3], r[4]
        got = parse(raw)
        if status == "OK":
            old_ok += 1
            if got:
                new_ok += 1
        else:
            inv += 1
            if got:
                rescued += 1
                if len(rescued_samples) < 4:
                    rescued_samples.append((r[0][11:], raw, sorted("%s/%s" % t for t in got)))
    print("%s: 原OK=%d  INVALID=%d" % (path, old_ok, inv))
    print("   -> 新正则仍能解析: %d 条;  从INVALID中额外救回: %d 条" % (new_ok, rescued))
    for t, raw, got in rescued_samples:
        print("      [%s] %s  ->  %s" % (t, raw[:44], ",".join(got)))
    print()

print("关键用例失败数:", bad)
