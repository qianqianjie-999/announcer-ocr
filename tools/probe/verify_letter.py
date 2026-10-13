#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""验证窗口字母开关(用带 C 的窗口)"""
import json, urllib.request

def probe(ov, win):
    body = {"number": "GA200", "window": win}
    if ov:
        body["tts"] = ov
    req = urllib.request.Request(
        "http://127.0.0.1:8080/api/test_tts", method="POST",
        data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    return json.load(urllib.request.urlopen(req, timeout=120))

r = probe({"repeat": 1}, "C04")
print("默认(不念字母) text =", r.get("text"))
r2 = probe({"repeat": 1, "speak_window_letter": True}, "C04")
print("勾选(念字母)   text =", r2.get("text"))
