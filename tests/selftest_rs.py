#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""selftest_rs.py — announce-rs 六阶段离线回放自测台【在 Debian 靶机上运行】

对齐 selftest.py(Python版) 的六阶段期望, 被测对象换成 Rust 二进制:

    变化检测 -> tesseract OCR -> 正则解析 -> 多轮确认 -> 去重 -> FIFO队列 -> (静音)播报

阶段序列(与 selftest.py 相同):
    空场启动 -> 举卡A(2号) -> 重复卡A(静默) -> 换卡B(新1号) -> 重复卡B(静默) -> 拿开卡(静默)

同时采样 announce-rs 主进程 VmRSS 峰值, 用于与 Python 版(onnxruntime ~700MB)对比。

前置条件(部署脚本已准备好):
  - 阶段帧 <dir>/phase_0.jpg .. phase_5.jpg (0空场 1卡A 2卡A 3卡B 4卡B 5空场)
  - 二进制 <bin> 与 tesseract 已就绪

用法: python3 selftest_rs.py <bin路径> <阶段帧目录>
"""
import json
import os
import shutil
import subprocess
import sys
import threading
import time

W, H = 1920, 1080
PHASES = [
    ("空场启动(等首帧就绪)", 0, 12, set()),
    ("举卡A  test_card.jpg", 1, 12, {("GA999", "C01"), ("GA200", "C08")}),
    ("重复同一张卡A(应静默)", 2, 8, set()),
    ("换成卡B  test_card_B.jpg", 3, 12, {("GA777", "C03")}),
    ("重复同一张卡B(应静默)", 4, 8, set()),
    ("拿开卡(回空场, 应静默)", 5, 8, set()),
]


def read_ok(logfile):
    """读取日志里已成功的 (号码, 窗口) 列表(按出现顺序)。"""
    if not os.path.exists(logfile):
        return []
    out = []
    with open(logfile, encoding="utf-8-sig", errors="replace") as f:
        for line in f:
            parts = line.rstrip("\r\n").split(",")
            if len(parts) >= 5 and parts[-1].strip() == "OK":
                out.append((parts[1], parts[2]))
    return out


def build_config(sandbox):
    cfg = {
        "camera_source": "simulate",
        "capture_width": W, "capture_height": H,
        "fps": 3, "low_latency": True,
        "roi": [0, 0, W, H],
        "lock_port": 51897,  # ★不能与生产实例相同: 生产 config.json 现用 51899,
                             #  撞端口会让沙盒实例起不来(表现为"空场通过、有号全失败")
        "log_file": "selftest_log.csv",
        "diag_heartbeat": True, "diag_every_sec": 5.0,
        "save_trigger_frames": True,
        "detect": {
            "diff_threshold": 25, "changed_ratio": 0.0015,
            "fallback_ocr_sec": 3.0, "stable_frames": 2,
            "confirm_ttl_sec": 4.0, "active_sec": 3.0,
            "min_interval_sec": 0.5,
        },
        "ocr": {
            "bin": "tesseract", "lang": "chi_sim", "psm": 11, "dpi": 150,
            "pattern": [
                "(?:请)?\\s*([A-Z]{1,2}\\d{3,4})\\s*[到至]\\s*([A-Z]{1,2}\\d{1,2})\\s*[号窗口]",
                "(?:请)?\\s*([A-Z]{1,2}\\d{3,4})\\s*[到至]\\s*([A-Z]?\\d{1,2})\\s*[号窗口]",
                "(?:请)?\\s*([A-Z]{1,2}\\d{3,4})\\s*[到至]\\s*([A-Z]{1,2}\\d{1,2})",
            ],
        },
        "tts": {"enabled": False, "repeat": 2, "template": "请{number}到{window}号窗口办理",
                "spell_number": False, "gap_sec": 0.35},
    }
    with open(os.path.join(sandbox, "config.json"), "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False, indent=2)


class RssSampler(threading.Thread):
    """每 1s 采一次 /proc/<pid>/status 的 VmRSS, 记录峰值(KB)。"""

    def __init__(self, pid):
        super().__init__(daemon=True)
        self.pid = pid
        self.max_kb = 0
        self.stop = False

    def run(self):
        path = "/proc/%d/status" % self.pid
        while not self.stop:
            try:
                with open(path) as f:
                    for ln in f:
                        if ln.startswith("VmRSS:"):
                            kb = int(ln.split()[1])
                            self.max_kb = max(self.max_kb, kb)
                            break
            except OSError:
                pass
            time.sleep(1.0)


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    bin_path = os.path.abspath(sys.argv[1])
    phase_dir = os.path.abspath(sys.argv[2])
    if not os.path.exists(bin_path):
        print("找不到二进制: " + bin_path)
        return 2

    root = os.path.dirname(bin_path)
    sandbox = os.path.join(root, "_rs_selftest")
    if os.path.exists(sandbox):
        shutil.rmtree(sandbox)
    os.makedirs(sandbox)
    logfile = os.path.join(sandbox, "selftest_log.csv")
    runlog_path = os.path.join(sandbox, "rs_run.log")
    frame = os.path.join(sandbox, "sim_frame.jpg")

    print("=" * 72)
    print("announce-rs 六阶段回放自测 — 二进制: %s" % bin_path)
    print("沙盒: %s" % sandbox)
    print("=" * 72)

    # 阶段0(空场)先就位, 再启动进程
    shutil.copy(os.path.join(phase_dir, "phase_0.jpg"), frame)
    build_config(sandbox)

    runlog = open(runlog_path, "w")
    proc = subprocess.Popen([bin_path], cwd=sandbox, stdout=runlog,
                            stderr=subprocess.STDOUT)
    sampler = RssSampler(proc.pid)
    sampler.start()

    results = []
    try:
        for name, ph, wait, expect in PHASES:
            before = set(read_ok(logfile))
            # 原子替换帧: 先写临时再 os.replace, 避免读到半张图
            tmp = os.path.join(sandbox, "_frame_tmp.jpg")
            shutil.copy(os.path.join(phase_dir, "phase_%d.jpg" % ph), tmp)
            os.replace(tmp, frame)
            time.sleep(wait)
            after = set(read_ok(logfile))
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
        sampler.stop = True
        runlog.close()

    order = read_ok(logfile)
    print("\n" + "=" * 72)
    print("完整播报序列(按日志顺序): %s" % (order,))
    dup = len(order) != len(set(order))
    print("去重检查: %s" % ("失败 — 有重复播报!" if dup else "通过 — 无重复"))
    print("号码总数: %d (期望 3: GA999/C01, GA200/C08, GA777/C03)" % len(order))

    passed = sum(1 for _, _, _, o in results if o)
    print("\n阶段结果: %d/%d 通过" % (passed, len(results)))
    print("announce-rs 主进程 RSS 峰值: %.1f MB" % (sampler.max_kb / 1024.0))
    print("详细运行日志: %s" % runlog_path)
    print("=" * 72)
    return 0 if passed == len(results) and not dup and len(order) == 3 else 1


if __name__ == "__main__":
    sys.exit(main())
