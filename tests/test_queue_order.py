# -*- coding: utf-8 -*-
"""test_queue_order.py — 验证"播报过程中屏幕再变化, 是否按顺序播报"
打桩替换TTS(每次播报耗时1.2秒), 在播报进行中陆续塞入新号, 检查播放顺序与是否漏播。
"""
import importlib.util
import json
import time

spec = importlib.util.spec_from_file_location("ann", "announce.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

played = []


def fake_make_tts_fn(cfg):
    def fn(text, times):
        for _ in range(times):
            time.sleep(1.2)              # 模拟一遍真实播报耗时
            played.append(text)
    fn.__name__ = "fake"
    return fn


m.make_tts_fn = fake_make_tts_fn          # 打桩: 不发真实声音

cfg = json.load(open("config.json", encoding="utf-8"))
a = m.Announcer(cfg)
time.sleep(0.3)

t0 = time.time()
a.speak("GA999", "C01")
time.sleep(0.4)                            # 第1条正在播出中
a.speak("GA777", "C02")                    # 播报中来了新号
time.sleep(0.4)
a.speak("GA555", "C03")                    # 又来了一个
a.queue.join()

expect = ["请GA999到C01号窗口办理"] * 2 + ["请GA777到C02号窗口办理"] * 2 + ["请GA555到C03号窗口办理"] * 2
print("实际播放顺序:")
for i, t in enumerate(played, 1):
    print(f"  {i}. {t}")
print(f"\n总耗时: {time.time() - t0:.1f} s  (3条×2遍×1.2s ≈ 7.2s, 说明确实是排队串行播的)")
print(f"顺序正确且无漏播: {'✅ 是' if played == expect else '❌ 否'}")
