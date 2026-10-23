#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""tts_server.py HTTP 层单元测试（纯标准库，注入假 piper 不依赖真模型）。

覆盖 2026-10-22 低危修复的请求体上限逻辑:
  * Content-Length > MAX_BODY_BYTES -> 413 且断开连接(keep-alive 不能复用)
  * Content-Length 非数字 -> 400
  * 正常合成 / 空 body / /health 不受影响

piper 真模型 63MB 且本机未必装包, 注入假模块后 import tts_server 即可
(顶层 from piper import PiperVoice 由假模块接住, VOICE 用假实现)。

跑法: python3 -m pytest tests/test_tts_server.py -q
     或  python3 tests/test_tts_server.py
"""
import json
import sys
import os
import types
import unittest
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

_TESTS_DIR = os.path.dirname(os.path.abspath(__file__))
_DEPLOY_DIR = os.path.join(os.path.dirname(_TESTS_DIR), "deploy")

# ---- 在 import tts_server 之前注入假 piper 模块 ----
if "piper" not in sys.modules:
    _fake_piper = types.ModuleType("piper")

    class _FakeVoice:
        def synthesize_wav(self, text, w):
            # wave 写帧前 channels/sampwidth/framerate 三件套都要设
            w.setnchannels(1)
            w.setsampwidth(2)
            w.setframerate(16000)
            w.writeframes(b"\x00\x00" * 32)

    _fake_piper.PiperVoice = types.SimpleNamespace(load=lambda model: _FakeVoice())
    sys.modules["piper"] = _fake_piper

if _DEPLOY_DIR not in sys.path:
    sys.path.insert(0, _DEPLOY_DIR)

import tts_server as ts  # noqa: E402


class TestTtsServerBodyLimit(unittest.TestCase):
    """请求体上限 + 非法 Content-Length 的 HTTP 层行为。"""

    @classmethod
    def setUpClass(cls):
        ts.VOICE = _fake_piper.PiperVoice.load("")
        cls.srv = ThreadingHTTPServer(("127.0.0.1", 0), ts.Handler)
        cls.base = "http://127.0.0.1:%d" % cls.srv.server_address[1]
        import threading
        threading.Thread(target=cls.srv.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.srv.shutdown()

    def _post(self, body=b"", headers=None):
        req = urllib.request.Request(self.base + "/", method="POST", data=body,
                                     headers=headers or {})
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return r.status, dict(r.headers)
        except urllib.error.HTTPError as e:
            return e.code, dict(e.headers)

    def test_normal_synth_200(self):
        code, hdrs = self._post(b"GA200")
        self.assertEqual(code, 200)
        self.assertEqual(hdrs.get("Content-Type"), "audio/wav")
        self.assertIn(hdrs.get("X-TTS-Cache"), ("hit", "miss"))

    def test_oversized_body_413(self):
        # > 4096 字节: 413 拒绝, 且必须断开连接(body 未读, keep-alive 不能复用)
        code, _ = self._post(b"", {"Content-Length": str(ts.MAX_BODY_BYTES + 1)})
        self.assertEqual(code, 413)

    def test_non_numeric_length_400(self):
        code, _ = self._post(b"", {"Content-Length": "abc"})
        self.assertEqual(code, 400)

    def test_empty_body_200_empty_wav(self):
        code, _ = self._post(b"")
        self.assertEqual(code, 200)

    def test_health_ok(self):
        with urllib.request.urlopen(self.base + "/health", timeout=10) as r:
            d = json.loads(r.read().decode())
        self.assertTrue(d["ok"])

    def _raw(self, req: bytes) -> bytes:
        import socket
        host, port = self.srv.server_address
        with socket.create_connection((host, port), timeout=5) as s:
            s.sendall(req)
            chunks = []
            while True:
                try:
                    b = s.recv(65536)
                except socket.timeout:
                    break
                if not b:
                    break
                chunks.append(b)
        return b"".join(chunks)

    def test_negative_content_length_400(self):
        # ★回归(2026-10-23 第八轮中危): int("-1") 合法, 旧版漏查 -> read(-1)
        # 语义是"读到 EOF", keep-alive 下线程永久挂起。必须 400 拒绝。
        # (urllib 会用 data 长度覆盖 Content-Length, 只能 raw socket 构造)
        req = (b"POST / HTTP/1.1\r\nHost: t\r\nConnection: close\r\n"
               b"Content-Length: -1\r\n\r\n")
        resp = self._raw(req)
        status = int(resp.split(b"\r\n", 1)[0].split()[1])
        self.assertEqual(status, 400, resp[:60])

    def test_cache_clear_resets_bytes_and_cache(self):
        # ★回归(2026-10-23 第八轮中危): /cache/clear 必须清 CACHE_BYTES,
        # 否则清空后第一次 cache_put 就触发 LRU 逐出, 新句被立刻挤掉。
        self._post(b"GA888")          # 合成一句 -> CACHE_BYTES > 0
        self.assertGreater(ts.CACHE_BYTES, 0)
        self.assertGreater(len(ts.CACHE), 0)
        # /cache/clear 是独立路径(空 body)
        req = urllib.request.Request(self.base + "/cache/clear",
                                     method="POST", data=b"")
        with urllib.request.urlopen(req, timeout=10) as r:
            self.assertEqual(r.status, 200)
        self.assertEqual(len(ts.CACHE), 0)
        self.assertEqual(ts.CACHE_BYTES, 0,
                         "清缓存后字节计数必须归零, 否则新句会被立即逐出")


if __name__ == "__main__":
    unittest.main(verbosity=2)
