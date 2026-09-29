# -*- coding: utf-8 -*-
"""监视 announce.py 的实时心跳与播报日志。

用途: 排查"举了卡却没反应"时, 判断到底是
  (a) 画面根本没变化  -> 心跳一直 0.0000
  (b) 有变化但没解析出 -> 心跳跳高, 但日志是 INVALID
  (c) 正常播报         -> 心跳跳高, 日志 OK

用法: python watch.py [最长分钟数]
"""
import os
import sys
import time

BASE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(BASE)                 # 项目根目录(日志所在地)
RUN = os.path.join(ROOT, "run.log")
LOG = os.path.join(ROOT, "announce_log.csv")

minutes = float(sys.argv[1]) if len(sys.argv) > 1 else 10.0
deadline = time.time() + minutes * 60


def tail_new(path, pos):
    """读 path 中 pos 之后的新增内容, 返回 (新行列表, 新位置)。"""
    if not os.path.exists(path):
        return [], pos
    size = os.path.getsize(path)
    if size < pos:          # 文件被轮转/清空
        pos = 0
    if size == pos:
        return [], pos
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        f.seek(pos)
        chunk = f.read()
        pos = f.tell()
    return [l for l in chunk.splitlines() if l.strip()], pos


run_pos = os.path.getsize(RUN) if os.path.exists(RUN) else 0
log_pos = os.path.getsize(LOG) if os.path.exists(LOG) else 0

print("监视中(最长 %.0f 分钟): 心跳变化 + 播报记录" % minutes, flush=True)
print("判读: 举卡时应出现 [监测] 最大帧差>阈值; 若一直 0.0000 说明画面无变化。", flush=True)

n_ok = n_invalid = n_change = n_quiet = 0
last_alive = time.time()
while time.time() < deadline:
    time.sleep(2)
    lines, run_pos = tail_new(RUN, run_pos)
    for l in lines:
        if "最大帧差" in l:
            n_change += 1
            # 只打"有变化"的行。静默行一分钟几十条, 全打会把真正的信号淹掉
            # (实测 30 分钟窗口刷了 333 行 0.0000, 没法看)。
            if "有变化" in l:
                print(l, flush=True)
            else:
                n_quiet += 1
        elif "存证" in l:
            print("  " + l, flush=True)
        elif "错误" in l or "Traceback" in l or "警告" in l:
            print("!! " + l, flush=True)

    lines, log_pos = tail_new(LOG, log_pos)
    for l in lines:
        if l.endswith("OK"):
            n_ok += 1
            print(">> 成功: " + l, flush=True)
        elif "INVALID" in l:
            n_invalid += 1
            print("   无效: " + l, flush=True)

    # 每隔 2 分钟报一次"还活着", 免得静默被误判成程序掉了
    if time.time() - last_alive >= 120:
        print("· 静默中(累计 %d 次心跳, 均无变化) %s"
              % (n_quiet, time.strftime("%H:%M:%S")), flush=True)
        last_alive = time.time()

    if n_ok >= 6:
        print("=== 已收到 6 条成功播报, 监视结束 ===", flush=True)
        break

print("--- 监视结束: 心跳 %d 次(变化 %d / 静默 %d), 成功 %d 条, 无效 %d 条 ---"
      % (n_change, n_change - n_quiet, n_quiet, n_ok, n_invalid), flush=True)
