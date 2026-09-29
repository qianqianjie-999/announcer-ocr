#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""check_host.py — 叫号屏 OCR 播报系统 · 目标主机体检（部署前先跑这个）

在准备部署的那台小主机上运行：
    cd <项目目录>
    python3 check_host.py            # 用自带的测试图测真实 OCR 耗时
    python3 check_host.py 图片路径    # 指定测试图

体检内容：
  1. 系统 / CPU / 内存
  2. 指令集支持（AVX/AVX2 有无，直接决定推理速度）
  3. 运算基准，与 i5-9400F 对比
  4. onnxruntime 能否运行（子进程隔离，捕捉 SIGILL 段错误）
  5. 实测单次 OCR 耗时 + 推算"屏幕跳号→开始播报"延迟
  6. 结论与参数建议
"""
import glob
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import time

PY = sys.executable
W = 68
REF_MATMUL = 0.00028      # i5-9400F(6核 i5) 上 300x300 float32 matmul 最快值(秒)，作标尺
DEFAULT_IMG = ["sim1.jpg", "sim_cardA.jpg", "test_card.jpg", "sim_cardB.jpg"]


def hr(t=""):
    print("=" * W)
    if t:
        print(t)
        print("=" * W)


def sec(t):
    print()
    print("-" * W)
    print(t)
    print("-" * W)


def run_isolated(code, timeout=120):
    """子进程隔离执行: 老CPU缺指令集时会 SIGILL 直接杀进程, 必须隔离子进程才测得出来"""
    try:
        p = subprocess.run([PY, "-c", code], capture_output=True, text=True,
                           timeout=timeout)
        return p.returncode, (p.stdout or "") + (p.stderr or "")
    except subprocess.TimeoutExpired:
        return 999, "超时(超过 %ds)" % timeout


hr("叫号屏 OCR 播报系统 · 目标主机体检")
print("时间:", time.strftime("%Y-%m-%d %H:%M:%S"))

# ---------------------------------------------------------------- 1
sec("1. 系统 / CPU / 内存")
print("系统   :", platform.platform())
print("架构   :", platform.machine())
print("Python :", sys.version.split()[0])

info, flags, cores, model = "", "", "?", "未知"
if os.path.exists("/proc/cpuinfo"):
    info = open("/proc/cpuinfo", encoding="utf-8", errors="ignore").read()
    m = re.search(r"model name\s*:\s*(.+)", info)
    if m:
        model = m.group(1).strip()
    cores = str(info.count("processor"))
    f = re.search(r"^flags\s*:\s*(.+)", info, re.M)
    flags = f.group(1) if f else ""
else:
    model = platform.processor() or "未知"
    cores = str(os.cpu_count())

print("CPU    :", model)
print("核心数 :", cores)

mem_total = mem_avail = None
if os.path.exists("/proc/meminfo"):
    mk = open("/proc/meminfo", encoding="utf-8").read()
    mt = re.search(r"MemTotal:\s+(\d+) kB", mk)
    ma = re.search(r"MemAvailable:\s+(\d+) kB", mk)
    if mt:
        mem_total = int(mt.group(1)) / 1024 / 1024
    if ma:
        mem_avail = int(ma.group(1)) / 1024 / 1024
if mem_total:
    print("内存   : 总 %.1f GB / 可用 %.1f GB" % (mem_total, mem_avail or 0))
    sw = re.search(r"SwapTotal:\s+(\d+) kB", open("/proc/meminfo", encoding="utf-8").read())
    if sw:
        print("交换区 : %.1f GB" % (int(sw.group(1)) / 1024 / 1024))
else:
    print("内存   : 读取失败")

try:
    du = shutil.disk_usage(os.path.abspath("."))
    print("磁盘   : 可用 %.1f GB" % (du.free / 1024 ** 3))
except Exception:
    pass

# ---------------------------------------------------------------- 2
sec("2. 指令集支持（直接决定推理速度）")
ISA = [("sse4_2", "基础"), ("avx", "重要"), ("avx2", "关键"), ("fma", "重要"), ("avx512f", "锦上添花")]
if flags:
    for k, note in ISA:
        print("  %-8s %s  (%s)" % (k, "✅ 支持" if k in flags else "❌ 不支持", note))
    if "avx2" not in flags:
        print("\n  ⚠ 无 AVX2: 深度学习推理只能退到 SSE2 内核, 通常比同频 AVX2 机器慢 2~4 倍")
else:
    print("  非 Linux 或无 /proc/cpuinfo, 跳过")

# ---------------------------------------------------------------- 3
sec("3. 运算基准（与 i5-9400F 标尺对比）")
score = None
try:
    import numpy as np
    a = np.random.rand(300, 300).astype("float32")
    b = np.random.rand(300, 300).astype("float32")
    for _ in range(3):
        np.dot(a, b)
    ts = []
    for _ in range(10):
        t = time.time()
        np.dot(a, b)
        ts.append(time.time() - t)
    best = min(ts)
    score = REF_MATMUL / best
    print("  本机 matmul: %.2f ms    → 相对 i5-9400F 约 %.2f 倍" % (best * 1000, score))
    print("  （注：这是多线程 BLAS 基准，只作粗略标尺；无 AVX 的老 CPU 在卷积网络上会比这个倍数更吃亏）")
except Exception as e:
    print("  numpy 不可用，跳过：", e)

# ---------------------------------------------------------------- 4
sec("4. onnxruntime 能否运行（子进程隔离测试，防段错误）")
ort_ok = False
rc, out = run_isolated("import onnxruntime as ort; print('ORT', ort.__version__)", timeout=90)
if rc == 0:
    ort_ok = True
    print("  ✅ 可以运行：", out.strip().splitlines()[-1] if out.strip() else "")
elif rc == -4 or "SIGILL" in out or "Illegal instruction" in out:
    print("  ❌ 崩溃：Illegal instruction (SIGILL) —— 该 CPU 缺少 onnxruntime 需要的指令集")
    print("     这条路走不通，OCR 需要换引擎（见第 6 节）")
elif rc == 999:
    print("  ⚠ 超时：", out)
else:
    print("  ❌ 导入失败 (rc=%s)：%s" % (rc, out.strip()[:300]))
    print("     若提示 No module named onnxruntime，请先跑 install.sh 装依赖")

# ---------------------------------------------------------------- 5
sec("5. 实测单次 OCR 耗时")
img_path = sys.argv[1] if len(sys.argv) > 1 else None
if not img_path:
    for c in DEFAULT_IMG:
        if os.path.exists(c):
            img_path = c
            break
roi = [100, 180, 1400, 650]
if os.path.exists("config.json"):
    try:
        roi = json.load(open("config.json", encoding="utf-8")).get("roi") or roi
        print("  读取 config.json 的 roi =", roi)
    except Exception:
        pass

ocr_ms = None
if not ort_ok:
    print("  ⏭ 跳过（onnxruntime 不可用）")
elif not img_path:
    print("  ⏭ 跳过（目录里没有测试图，把一个叫号屏截图命名为 sim1.jpg 放进来再跑）")
elif not os.path.exists(img_path):
    print("  ⏭ 跳过（图片不存在: %s）" % img_path)
else:
    print("  测试图:", img_path)
    code = (
        "import time, cv2\n"
        "from rapidocr_onnxruntime import RapidOCR\n"
        "eng = RapidOCR()\n"
        "img = cv2.imread(%r)\n"
        "roi = img[%d:%d, %d:%d]\n"
        "eng(roi)\n"
        "ts = []\n"
        "for _ in range(3):\n"
        "    t = time.time(); eng(roi); ts.append((time.time()-t)*1000)\n"
        "print('OCR_MS', round(sum(ts)/len(ts)))\n"
        % (img_path, roi[1], roi[1] + roi[3], roi[0], roi[0] + roi[2])
    )
    rc, out = run_isolated(code, timeout=600)
    mm = re.search(r"OCR_MS\s+(\d+)", out)
    if mm:
        ocr_ms = int(mm.group(1))
        print("  ✅ 单次 OCR: %d ms" % ocr_ms)
    else:
        print("  ❌ 测试失败 (rc=%s)" % rc)
        print("  输出尾部:", out.strip()[-300:])

# ---------------------------------------------------------------- 6
sec("6. 结论与建议")
if ocr_ms is None:
    print("未能测得 OCR 耗时，无法给出延迟推算。")
    print("建议：跑通 install.sh 后重试，或把报错发出来。")
else:
    open_delay = 0.5 + ocr_ms / 1000 * 2      # 取流轮询0.5s + 首次OCR + 确认1轮
    print("  单次 OCR          : %.2f 秒" % (ocr_ms / 1000))
    print("  预计开口延迟      : 约 %.1f 秒  (屏幕跳号 → 开始播报)" % open_delay)
    print("  其中确认占        : %.1f 秒  (stable_frames=2)" % (ocr_ms / 1000))
    print()
    if ocr_ms < 1000:
        print("  ✅ 很流畅 —— 直接用，参数保持默认")
    elif ocr_ms < 2000:
        print("  ✅ 良好 —— 叫号场景完全够用")
    elif ocr_ms < 4000:
        print("  ⚠ 勉强 —— 建议 fps=5、stable_frames=2、tts.repeat=1 压缩延迟")
    else:
        print("  ❌ 太慢 —— 开口延迟超过 8 秒，现场体验会很差，建议换机器")
        print("     推荐：Intel N100 迷你主机（¥600~1300），同场景约 0.8~1.5 秒/次")

    if mem_total and mem_total < 3:
        print()
        print("  内存偏小（%.1f GB）建议：%s" % (mem_total,
              "stable_frames=2 / tts.repeat=1 / 用 espeak-ng 而非 piper / 关掉桌面上其它服务"))
    if score is not None and score < 0.3:
        print()
        print("  运算能力仅为标尺的 %.0f%%，属于老平台。" % (score * 100))
        print("  若这台机器想派用处：跑网口嗅探(02_sniff.py)这类纯解析任务绰绰有余，")
        print("  功耗 10W 适合 7×24；但 OCR 推理是它的短板。")

print()
print("=" * W)
print("体检完毕。把上面结果发出来即可给出针对性参数。")
print("=" * W)
