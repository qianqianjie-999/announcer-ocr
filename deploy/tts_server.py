#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""常驻 TTS 合成服务（piper 中文）。

为什么常驻: piper CLI 每句都要重新加载 63MB 模型(约 6 秒), 常驻后模型只加载一次。
为什么加缓存: 靶机上同一句会被重复请求(号码被重复检测到、现场连点测试按钮),
             缓存命中直接返回上次 wav, 耗时从数秒降到毫秒。

实测(J1850 4 核, 与 OCR 并行): medium 合成 2.6~3.0s / x_low 1.3~1.6s。
注意本服务的 CPUQuota 必须放宽, 否则 onnxruntime 多线程被限流, 合成会翻倍变慢。

仅监听 127.0.0.1, 供本机 announce-rs 与 config_web 调用。
用法: python3 tts_server.py [--model xx.onnx] [--port 5001] [--host 127.0.0.1]
"""
import argparse
import io
import json
import threading
import time
import wave
from collections import OrderedDict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from piper import PiperVoice

ARGS = None
VOICE = None
LOCK = threading.Lock()

# 合成结果内存缓存(LRU): text -> wav 字节
CACHE = OrderedDict()
CACHE_BYTES = 0
CACHE_MAX_BYTES = 48 << 20          # 48MB; 单句 wav 约 120KB, 够存几百句
STATS = {"hit": 0, "miss": 0, "synth_n": 0, "synth_sec_total": 0.0, "last_synth_sec": 0.0}
# ★2026-10-22 中危修复: 缓存自用小锁 —— 旧版 cache_get/cache_put 在合成锁外跑,
# ThreadingHTTPServer 并发下 "命中后 move_to_end" 会与另一线程的 LRU 逐出竞态,
# key 恰被 popitem 挤出 => move_to_end 抛 KeyError => do_POST 500, 该句播报丢失。
# 与合成锁(LOCK)只单向嵌套(合成锁内可拿缓存锁, 反之不行), 无死锁。
CACHE_LOCK = threading.Lock()


def cache_get(key):
    with CACHE_LOCK:
        val = CACHE.get(key)
        if val is None:
            return None
        CACHE.move_to_end(key)          # LRU: 命中的挪到末尾
        STATS["hit"] += 1
        return val


def cache_put(key, val):
    global CACHE_BYTES
    with CACHE_LOCK:
        old = CACHE.pop(key, None)
        if old is not None:
            CACHE_BYTES -= len(old)
        CACHE[key] = val
        CACHE_BYTES += len(val)
        while CACHE_BYTES > CACHE_MAX_BYTES and len(CACHE) > 1:
            _, evicted = CACHE.popitem(last=False)
            CACHE_BYTES -= len(evicted)


def synthesize(text):
    """合成一句; 命中缓存则零耗时。返回 (wav 字节, 是否命中缓存)。"""
    hit = cache_get(text)
    if hit is not None:
        return hit, True
    with LOCK:                       # piper voice 非线程安全, 串行合成
        # 二次检查: 排队期间可能已被别的请求合成好
        hit = cache_get(text)
        if hit is not None:
            return hit, True
        STATS["miss"] += 1
        t0 = time.time()
        buf = io.BytesIO()
        with wave.open(buf, "wb") as w:
            VOICE.synthesize_wav(text, w)
        dt = time.time() - t0
        STATS["synth_n"] += 1
        STATS["synth_sec_total"] += dt
        STATS["last_synth_sec"] = dt
    data = buf.getvalue()
    cache_put(text, data)
    return data, False


# 请求体上限(字节): 一句播报模板几十个汉字 ≈ 60B, 4KB 宽裕百倍。
# ★2026-10-22 低危修复: 旧版对 Content-Length 不设限 —— 超大 body 会整段读进
# 内存, 且超长文本的合成要占住 LOCK 数十秒, 阻塞所有正常播报。
MAX_BODY_BYTES = 4096


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):  # 静默, systemd journal 只留启动与错误
        pass

    def _send(self, body, ctype, extra=None, code=200):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.startswith("/health"):
            avg = (STATS["synth_sec_total"] / STATS["synth_n"]) if STATS["synth_n"] else 0.0
            info = {
                "ok": True,
                "model": ARGS.model if ARGS else "",
                "cache_items": len(CACHE),
                "cache_mb": round(CACHE_BYTES / 1048576.0, 2),
                "hits": STATS["hit"],
                "misses": STATS["miss"],
                "synth_avg_sec": round(avg, 2),
                "synth_last_sec": round(STATS["last_synth_sec"], 2),
            }
            self._send(json.dumps(info, ensure_ascii=False).encode("utf-8"),
                       "application/json; charset=utf-8")
        else:
            self._send(b"tts server", "text/plain")

    def do_POST(self):
        try:
            try:
                n = int(self.headers.get("Content-Length", 0))
            except ValueError:
                self._send("Content-Length 非法".encode("utf-8"), "text/plain", code=400)
                return
            if n > MAX_BODY_BYTES:
                # body 没读就回 413: keep-alive 下连接不能复用, 必须断开重连
                self.close_connection = True
                self._send("请求体过大".encode("utf-8"), "text/plain", code=413)
                return
            text = self.rfile.read(n).decode("utf-8").strip()
            # 清缓存要放在 text 判空之前: 该请求自身 body 就是空的
            if self.path.startswith("/cache/clear"):
                with LOCK:
                    CACHE.clear()
                STATS["hit"] = STATS["miss"] = STATS["synth_n"] = 0
                STATS["synth_sec_total"] = STATS["last_synth_sec"] = 0.0
                self._send(b"cleared", "text/plain")
                return
            if not text:
                self._send(b"", "audio/wav")
                return
            t0 = time.time()
            data, hit = synthesize(text)
            self._send(data, "audio/wav", {
                "X-TTS-Cache": "hit" if hit else "miss",
                "X-TTS-Total-Ms": "%d" % int((time.time() - t0) * 1000),
            })
        except Exception as e:  # noqa: BLE001
            msg = ("合成失败: %s" % e).encode("utf-8")
            self.send_response(500)
            self.send_header("Content-Length", str(len(msg)))
            self.end_headers()
            self.wfile.write(msg)


def main():
    global ARGS, VOICE
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="/home/debian/anntest/tts/zh_CN-huayan-medium.onnx")
    ap.add_argument("--port", type=int, default=5001)
    ap.add_argument("--host", default="127.0.0.1")
    ARGS = ap.parse_args()
    VOICE = PiperVoice.load(ARGS.model)          # 一次性加载, 常驻内存
    print("[tts-server] 模型已加载: %s, 监听 %s:%d" % (ARGS.model, ARGS.host, ARGS.port),
          flush=True)
    ThreadingHTTPServer((ARGS.host, ARGS.port), Handler).serve_forever()


if __name__ == "__main__":
    main()
