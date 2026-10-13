#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""部署后验证: 二进制/页面/试听文本"""
import hashlib, json, urllib.request

BIN = "/opt/announce-rs/announce-rs"
SRC = "/home/debian13/build/target/release/announce-rs"
h = lambda p: hashlib.md5(open(p, "rb").read()).hexdigest()
print("md5 部署二进制 =", h(BIN))
print("md5 编译产物   =", h(SRC))
print("一致           =", h(BIN) == h(SRC))

# 页面
pg = urllib.request.urlopen("http://127.0.0.1:8080/", timeout=10).read().decode()
print("\n页面含 t_wl 勾选框 =", "t_wl" in pg)
print("页面含 不念字母 文案 =", "默认不勾" in pg)
print("页面含 语速框 t_len =", "t_len" in pg)

# 试听: 不带任何 override -> 用磁盘配置(speak_window_letter 缺省=false)
req = urllib.request.Request(
    "http://127.0.0.1:8080/api/test_tts", method="POST",
    data=json.dumps({}).encode(), headers={"Content-Type": "application/json"})
r = json.load(urllib.request.urlopen(req, timeout=120))
print("\n=== 试听(默认配置, GA200->08) ===")
print("ok   =", r.get("ok"))
print("text =", r.get("text"))
print("msg  =", r.get("msg"))

# 再试一次: override 念字母, 对比
req2 = urllib.request.Request(
    "http://127.0.0.1:8080/api/test_tts", method="POST",
    data=json.dumps({"tts": {"speak_window_letter": True, "repeat": 1}}).encode(),
    headers={"Content-Type": "application/json"})
r2 = json.load(urllib.request.urlopen(req2, timeout=120))
print("\n=== 试听(勾选念字母) ===")
print("text =", r2.get("text"))
