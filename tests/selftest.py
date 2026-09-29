# -*- coding: utf-8 -*-
"""selftest.py — 离线回放自测台（不需要摄像头、不需要举卡）

为什么要它: 现场验证依赖"有人在镜头前举卡", 一旦没人举就只能干等, 而且手抖会
引入动态模糊, 反而不如固定画面好判断。本脚本把 announce.py 放进沙盒里, 用
camera_source="simulate" 模式喂**合成帧序列**, 自动验证整条链路:

    变化检测 -> 触发OCR -> 正则解析 -> 多轮确认 -> 去重 -> FIFO队列 -> 播报

它验不到(必须真机): 摄像头对焦/曝光/现场光照、RTSP 延迟。

沙盒隔离措施(保证不打扰正在运行的生产实例):
  - 独立目录 _selftest/, 自带 config.json / 日志 / sim_frame.jpg
  - camera_source="simulate"  -> 不打开任何摄像头
  - lock_port=51899            -> 不抢生产实例的 51888
  - tts.enabled=false          -> 不出声, 只把播报文本打到 stdout

用法: python selftest.py
"""
import json
import os
import shutil
import subprocess
import sys
import time

import cv2
import numpy as np

BASE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(BASE)                     # 项目根目录(announce.py/config.json 所在地)
SANDBOX = os.path.join(ROOT, "_selftest")
FRAME = os.path.join(SANDBOX, "sim_frame.jpg")
LOGFILE = os.path.join(SANDBOX, "selftest_log.csv")
RUNLOG = os.path.join(SANDBOX, "selftest_run.log")
CARD_A = os.path.join(ROOT, "assets", "test_card.jpg")     # GA999->C01, GA200->C08
CARD_B = os.path.join(ROOT, "assets", "test_card_B.jpg")   # GA777->C03, GA999->C01

W, H = 1920, 1080


def imread_u(path):
    """读图 — 必须绕过 cv2.imread: Windows 下 OpenCV 用窄字符 fopen,
    项目路径含中文("叫号屏摄像头OCR")时 imread/imwrite 会静默失败。"""
    data = np.fromfile(path, dtype=np.uint8)
    if data.size == 0:
        return None
    return cv2.imdecode(data, cv2.IMREAD_COLOR)


def imwrite_u(path, img, quality=92):
    ok, buf = cv2.imencode(".jpg", img, [cv2.IMWRITE_JPEG_QUALITY, quality])
    if not ok:
        return False
    buf.tofile(path)
    return True


def compose(card_path=None):
    """合成一帧 1920x1080 画面: 黑底, 测试卡等比缩放居中贴上。
    card_path=None 表示纯黑(模拟"没举卡"的空场景)。"""
    canvas = np.zeros((H, W, 3), np.uint8)
    if card_path is None:
        return canvas
    img = imread_u(card_path)
    if img is None:
        raise SystemExit("读不到测试卡: " + card_path)
    ih, iw = img.shape[:2]
    scale = min(W / iw, H / ih)
    nw, nh = int(iw * scale), int(ih * scale)
    img = cv2.resize(img, (nw, nh), interpolation=cv2.INTER_AREA)
    x0, y0 = (W - nw) // 2, (H - nh) // 2
    canvas[y0:y0 + nh, x0:x0 + nw] = img
    return canvas


def write_frame(img):
    """原子式写帧: 先写临时再替换, 避免 announce.py 读到半张图。"""
    tmp = os.path.join(SANDBOX, "_frame_tmp.jpg")
    if not imwrite_u(tmp, img):
        raise SystemExit("写帧失败: " + tmp)
    os.replace(tmp, FRAME)


def read_ok():
    """读取日志里已成功的 (号码, 窗口) 列表(按出现顺序)。"""
    if not os.path.exists(LOGFILE):
        return []
    out = []
    with open(LOGFILE, encoding="utf-8-sig") as f:
        for line in f:
            parts = line.rstrip("\n").split(",")
            if len(parts) >= 5 and parts[-1] == "OK":
                out.append((parts[1], parts[2]))
    return out


def build_sandbox():
    if os.path.exists(SANDBOX):
        shutil.rmtree(SANDBOX)
    os.makedirs(SANDBOX)
    shutil.copy(os.path.join(ROOT, "announce.py"), SANDBOX)
    cfg = json.load(open(os.path.join(ROOT, "config.json"), encoding="utf-8"))
    cfg["camera_source"] = "simulate"        # 不打开摄像头
    cfg["lock_port"] = 51899                 # 不抢生产实例的 51888
    cfg["log_file"] = "selftest_log.csv"
    cfg["roi"] = [0, 0, W, H]
    cfg["tts"]["enabled"] = False            # 静音, 只看文本
    cfg["diag_heartbeat"] = True
    with open(os.path.join(SANDBOX, "config.json"), "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False, indent=2)
    return cfg


def main():
    print("=" * 72)
    print("离线回放自测台 — 沙盒: " + SANDBOX)
    print("=" * 72)
    cfg = build_sandbox()
    print("配置: camera_source=%s lock_port=%s tts.enabled=%s changed_ratio=%s"
          % (cfg["camera_source"], cfg["lock_port"], cfg["tts"]["enabled"],
             cfg["detect"]["changed_ratio"]))

    write_frame(compose(None))
    proc = subprocess.Popen(
        [sys.executable, "-u", "announce.py"],
        cwd=SANDBOX, stdout=open(RUNLOG, "w", encoding="utf-8"),
        stderr=subprocess.STDOUT)

    # 阶段定义: (名称, 帧内容, 等待秒数, 期望新增的OK集合)
    phases = [
        ("空场启动(等OCR模型加载)", compose(None), 12, set()),
        ("举卡A  test_card.jpg", compose(CARD_A), 9,
         {("GA999", "C01"), ("GA200", "C08")}),
        ("重复同一张卡A(应静默)", compose(CARD_A), 7, set()),
        ("换成卡B  test_card_B.jpg", compose(CARD_B), 9,
         {("GA777", "C03")}),
        ("重复同一张卡B(应静默)", compose(CARD_B), 7, set()),
        ("拿开卡(回空场, 应静默)", compose(None), 7, set()),
    ]

    results = []
    try:
        for name, img, wait, expect in phases:
            before = set(read_ok())
            write_frame(img)
            t0 = time.time()
            while time.time() - t0 < wait:
                time.sleep(0.5)
            after = set(read_ok())
            new = after - before
            ok = (new == expect)
            results.append((name, expect, new, ok))
            print("\n[%s] %s" % ("通过" if ok else "失败", name))
            print("    期望新增: %s" % (sorted(expect) or "无"))
            print("    实际新增: %s" % (sorted(new) or "无"))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()

    # 顺序检查: 卡A两号应各出现一次, 卡B新号在其后
    order = read_ok()
    print("\n" + "=" * 72)
    print("完整播报序列(按日志顺序): %s" % (order,))
    dup = len(order) != len(set(order))
    print("去重检查: %s" % ("失败 — 有重复播报!" if dup else "通过 — 无重复"))
    print("号码总数: %d (期望 3: GA999/C01, GA200/C08, GA777/C03)" % len(order))

    passed = sum(1 for _, _, _, o in results if o)
    print("\n阶段结果: %d/%d 通过" % (passed, len(results)))
    print("=" * 72)
    print("详细日志: %s" % RUNLOG)

    if passed == len(results) and not dup and len(order) == 3:
        print("\n结论: 软件链路全部正常。若真机仍无播报, 问题必然在【摄像头成像】")
        print("      (对焦/距离/光照), 而不在检测、识别、去重、播报逻辑。")
    return 0 if passed == len(results) and not dup else 1


if __name__ == "__main__":
    sys.exit(main())
