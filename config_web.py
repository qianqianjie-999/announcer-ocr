#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""config_web.py — announce-rs 配置器 Web UI（Python 标准库, 无 pip 依赖）

放 /home/debian/anntest/ 下, 与 config.json 同目录。
功能:
  * 配置页: 摄像头源(USB/RTSP 含 IP+密码)/分辨率/ROI 拖框/检测参数/播报参数
  * 抓帧预览: ffmpeg 从 RTSP 或 USB 设备抓一帧 JPEG(超时 15s)
  * ROI 拖框: 预览图上鼠标拖矩形, 坐标按抓帧分辨率换算
  * 保存: 校验 -> 原子写 config.json -> sudo systemctl restart announce-rs
    (sudoers 窄授权: /etc/sudoers.d/announce-web 仅放行 restart announce-rs)
端口 8080, 仅限内网使用, 无认证(调试场景)。
"""
import errno
import fcntl
import glob
import hashlib
import json
import math
import os
import re
import struct
import subprocess
import tempfile
import threading
import time
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = 8080
HERE = os.path.dirname(os.path.abspath(__file__))
CONFIG = os.path.join(HERE, "config.json")
SIM_FRAME = os.path.join(HERE, "sim_frame.jpg")
LOG_CSV = os.path.join(HERE, "announce_log.csv")
RESTART_CMD = ["sudo", "-n", "/usr/bin/systemctl", "restart", "announce-rs"]
# 工作时间调度：root 辅助脚本，负责「写 systemd timer + 按当前时刻校正运行状态」。
# 保存配置后优先走它（未启用调度时它会退化为 restart，等价旧行为）。
SCHED_CMD = ["sudo", "-n", "/usr/local/sbin/announce-schedule-apply"]
NET_SCRIPT = "/usr/local/sbin/announce-net-apply.sh"
# 手工校时: 配置台以 debian13 跑, 改系统时间要 root。现场无 sudo, 用 su 就得在
# 这里明文存 root 密码(不可接受) —— 改为"请求文件", 由 root 的 announce-time.timer
# 每 5 秒取走执行, 结果写回 result 供页面显示。见 announce-time-apply.sh。
TIME_DIR = "/var/lib/announce-rs/time"

NET_DEFAULT_IP = "192.168.31.1"  # 出厂默认(直连调试场景; 有路由器的现场勿用, 多为网关地址)

# ---------------------------------------------------------------- config io

CONFIG_LOCK = threading.Lock()


def load_cfg():
    with CONFIG_LOCK:
        with open(CONFIG, "r", encoding="utf-8") as f:
            return json.load(f)


def save_cfg(cfg):
    tmp = CONFIG + ".tmp"
    with CONFIG_LOCK:
        try:
            with open(tmp, "w", encoding="utf-8") as f:
                # ★allow_nan=False(2026-10-22 第三轮审查): nan/inf 一旦落盘成字面量
                #   NaN/Infinity, 主服务 serde_json 拒读 config.json → announce-rs
                #   启动即崩循环; Python json.load 又宽容读得回, 页面看不出异常。
                #   写出前就抛错(原子写保证旧配置原样), 异常路径清掉半截 tmp。
                json.dump(cfg, f, ensure_ascii=False, indent=2, allow_nan=False)
        except ValueError:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
        os.replace(tmp, CONFIG)  # 原子替换, 主服务/配置器永不读到半截文件


def config_rev(path=None):
    """配置内容指纹(sha256 前 16 hex), 乐观锁用: 内容没变 rev 就不变。
    不用 mtime 是因为 touch/复制也会动 mtime 却没改内容, 会造成假冲突。
    """
    p = path or CONFIG
    try:
        with open(p, "rb") as f:
            return hashlib.sha256(f.read()).hexdigest()[:16]
    except OSError:
        return ""


# ---------------------------------------------------------------- ffmpeg snapshot

def ffmpeg_snapshot(src, w, h):
    """按源类型抓一帧 JPEG, 返回 (bytes|None, err信息)。src 已是后端语义。"""
    if src == "simulate":
        try:
            with open(SIM_FRAME, "rb") as f:
                return f.read(), None
        except OSError as e:
            return None, "simulate 源无 %s: %s" % (SIM_FRAME, e)
    if src.startswith("rtsp://") or src.startswith("rtsp:"):
        # ★纵深防御: 即便上层漏检, 这里也拦一道(见 validate_rtsp 注释)
        ok, src_or_msg = validate_rtsp(src)
        if not ok:
            return None, "非法 RTSP 源: " + src_or_msg
        src = src_or_msg
        cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-rtsp_transport", "tcp",
               "-i", src, "-frames:v", "1", "-q:v", "4", "-f", "image2", "pipe:1"]
    elif src.isdigit():
        cmd = ["ffmpeg", "-hide_banner", "-loglevel", "error", "-f", "v4l2",
               "-video_size", "%dx%d" % (w, h), "-i", "/dev/video" + src,
               "-frames:v", "1", "-q:v", "4", "-f", "image2", "pipe:1"]
    else:
        return None, "不支持的源: %r" % src
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=15)
    except subprocess.TimeoutExpired:
        return None, "抓帧超时(15s): 检查摄像头 IP/账号密码/网络可达"
    if p.returncode != 0 or not p.stdout:
        tail = p.stderr.decode("utf-8", "replace").strip().splitlines()
        return None, "ffmpeg 失败: " + (tail[-1] if tail else ("code %s" % p.returncode))
    return p.stdout, None


def status_and_log():
    st = {"active": "?", "pid": "-", "mem": "-"}
    try:
        # 注意: --property 用逗号形式一次传齐; 分开写 -p 会把后续属性名当位置参数解析错
        out = subprocess.run(
            ["systemctl", "show", "announce-rs", "--no-pager",
             "--property=ActiveState,SubState,MainPID,MemoryCurrent"],
            capture_output=True, timeout=5).stdout.decode()
        for line in out.splitlines():
            k, _, v = line.partition("=")
            if k == "ActiveState":
                st["active"] = v
            elif k == "MainPID":
                st["pid"] = v
            elif k == "MemoryCurrent" and v.isdigit():
                st["mem"] = "%.1f MB" % (int(v) / 1048576)
    except Exception as e:  # noqa: BLE001 — 状态拿不到不应带崩配置页
        st["err"] = str(e)
    tail = ""
    try:
        with open(LOG_CSV, "r", encoding="utf-8", errors="replace") as f:
            tail = "".join(f.readlines()[-15:])
    except OSError:
        pass
    return st, tail


def norm_src(src):
    """画面源归一化: '/dev/videoN' -> 'N'。

    Rust 版 config 的 camera_source 允许设备路径写法(见 cfg.rs source()),
    配置台其余逻辑只认数字索引——不归一化的话, 页面默认抓帧(不带 src 参数,
    直接用配置里的 camera_source)会误报"非法源"(2026-10-07 实测)。
    """
    src = (src or "").strip()
    m = re.match(r"^/dev/video(\d+)$", src)
    return m.group(1) if m else src


def announce_running():
    """主程序进程是否在跑（不依赖 systemd）。

    本地调试/应急场景常用 nohup 手工拉起 announce-rs, systemctl 查到 inactive,
    会让 USB 抓帧误走"自己开 ffmpeg"分支 -> 撞上 Device busy(2026-10-07 实测)。
    pgrep 直接看进程名, systemd 拉起与手工拉起都能探到。
    """
    try:
        return subprocess.run(["pgrep", "-x", "announce-rs"],
                              capture_output=True, timeout=5).returncode == 0
    except Exception:  # noqa: BLE001 — 探测失败按"没在跑"处理, 走直接抓帧
        return False


# ---------------------------------------------------------------- logs & disk

def _run_quiet(cmd, timeout=8):
    """跑一条只读命令, 失败一律返回空串(面板不该因为某个命令不存在就整个报错)。"""
    try:
        p = subprocess.run(cmd, capture_output=True, timeout=timeout)
        return p.stdout.decode("utf-8", "replace").strip()
    except Exception:  # noqa: BLE001
        return ""


def logs_status():
    """日志/存证/磁盘占用总览 —— 供配置台「日志与磁盘」面板显示, 也可直接 curl 查。"""
    def fsize(p):
        try:
            return os.path.getsize(p)
        except OSError:
            return 0

    out = {"csv_bytes": fsize(LOG_CSV), "csv_lines": 0, "csv_first": "", "csv_last": ""}
    try:
        with open(LOG_CSV, "r", encoding="utf-8-sig", errors="replace") as f:
            lines = f.readlines()
        out["csv_lines"] = max(0, len(lines) - 1)  # 首行是表头(带 BOM)
        if len(lines) > 1:
            out["csv_first"] = lines[1].split(",")[0].strip().strip('"')
            out["csv_last"] = lines[-1].split(",")[0].strip().strip('"')
    except OSError:
        pass

    live = os.path.join(HERE, "_live")
    try:
        trigs = [f for f in os.listdir(live) if f.startswith("trig_") and f.endswith(".jpg")]
    except OSError:
        trigs = []
    out["trig_count"] = len(trigs)
    out["trig_bytes"] = sum(fsize(os.path.join(live, f)) for f in trigs)

    arch = os.path.join(HERE, "logs_archive")
    try:
        files = sorted((f for f in os.listdir(arch)
                        if f.startswith("announce_log_") and f.endswith(".csv")), reverse=True)
    except OSError:
        files = []
    out["archive_count"] = len(files)
    out["archive"] = [{"name": f, "kb": round(fsize(os.path.join(arch, f)) / 1024.0, 1)}
                      for f in files[:8]]

    try:
        st = os.statvfs(HERE)
        total = st.f_blocks * st.f_frsize
        used = (st.f_blocks - st.f_bfree) * st.f_frsize
        out["disk"] = {"total_mb": int(total // 1048576), "used_mb": int(used // 1048576),
                       "avail_mb": int(st.f_bavail * st.f_frsize // 1048576),
                       "pct": int(used * 100 / total) if total else 0}
    except OSError:
        out["disk"] = {}

    j = _run_quiet(["journalctl", "--disk-usage"])
    m = re.search(r"take up ([0-9.]+[KMG]?)", j)
    out["journal"] = m.group(1) if m else j[:60]
    out["timer_enabled"] = _run_quiet(["systemctl", "is-enabled", "announce-cleanup.timer"])
    nxt = _run_quiet(["systemctl", "show", "announce-cleanup.timer",
                      "--property=NextElapseUSecRealtime", "--value"])
    out["timer_next"] = nxt[:19].replace("T", " ") if nxt and nxt != "n/a" else ""
    return out


def logs_action(action):
    """配置台的手动清理（只用 debian 自身权限, 不依赖 sudo —— 本机 debian 不在 sudoers）。

    完整清理含 journal 收缩, 需要 root, 由每日 03:30 的 announce-cleanup.timer 自动做；
    这里提供的是现场最常用的两个动作, 都严格限定在项目目录内的白名单文件名。
    """
    live = os.path.join(HERE, "_live")
    arch = os.path.join(HERE, "logs_archive")

    if action == "archive_csv":
        if not os.path.exists(LOG_CSV) or os.path.getsize(LOG_CSV) == 0:
            return False, "播报日志已经是空的，无需归档"
        try:
            os.makedirs(arch, exist_ok=True)
            dst = os.path.join(arch, "announce_log_%s.csv" % time.strftime("%Y%m%d_%H%M%S"))
            os.replace(LOG_CSV, dst)  # 原子搬家: 主程序下次写入会自动新建并补 BOM
            open(LOG_CSV, "wb").close()
        except OSError as e:
            return False, "归档失败: %s" % e
        return True, "已归档并清空（%.1f KB → logs_archive/%s）" % (
            os.path.getsize(dst) / 1024.0, os.path.basename(dst))

    if action == "clear_trig":
        n = 0
        try:
            names = os.listdir(live)
        except OSError:
            return False, "无 _live 目录"
        for f in names:
            if f.startswith("trig_") and f.endswith(".jpg"):
                try:
                    os.remove(os.path.join(live, f))
                    n += 1
                except OSError:
                    pass
        return True, "已清理 %d 张存证截图（下次触发播报会重新生成）" % n

    return False, "未知操作: %r" % action


# ---------------------------------------------------------------- network

def net_status():
    """返回 {ip, prefix, gw, pending}; 出错字段留空。"""
    st = {"ip": "", "prefix": "", "gw": "", "pending": False}
    try:
        out = subprocess.run(["sudo", "-n", NET_SCRIPT, "status"],
                             capture_output=True, timeout=10).stdout.decode()
        rows = [l.strip() for l in out.splitlines() if l.strip()]
        if rows and "/" in rows[0]:
            ip, _, pfx = rows[0].partition("/")
            st["ip"], st["prefix"] = ip, pfx
        if len(rows) > 1 and rows[1] and "." in rows[1]:
            st["gw"] = rows[1]
        st["pending"] = "PENDING" in rows
    except Exception as e:  # noqa: BLE001 — 状态拿不到不应带崩配置页
        st["err"] = str(e)
    return st


def net_apply(body):
    """apply 新网络配置(带90s回滚) 或 factory。返回 (ok, msg)。"""
    if body.get("factory"):
        args = ["sudo", "-n", NET_SCRIPT, "factory"]
    else:
        ip = str(body.get("ip", "")).strip()
        gw = str(body.get("gw", "")).strip() or "-"
        dns = str(body.get("dns", "")).strip() or "-"
        try:
            prefix = int(body.get("prefix", 24))
        except (TypeError, ValueError):
            return False, "掩码前缀非法"
        # 后端前置校验(脚本内还有白名单硬校验)
        if not ip.replace(".", "").isdigit() or not ip:
            return False, "IP 格式非法"
        if not (8 <= prefix <= 32):
            return False, "掩码前缀须在 8~32"
        args = ["sudo", "-n", NET_SCRIPT, "apply", ip, str(prefix), gw, dns]
    try:
        p = subprocess.run(args, capture_output=True, timeout=60)
    except subprocess.TimeoutExpired:
        return False, "网络配置命令超时——若页面失联, 请改用新 IP 访问(90秒不确认会自动回滚)"
    out = (p.stdout + p.stderr).decode("utf-8", "replace").strip()
    if p.returncode != 0:
        return False, out or ("code %s" % p.returncode)
    return True, out


# ---------------------------------------------------------------- 校验

# ---------------------------------------------------------------- 安全校验

# RTSP 地址白名单正则：只允许标准摄像头 URL 字符集。
# ★为什么必须严格校验（安全事件复盘 2026-09-28）：
#   RTSP 地址会作为 `ffmpeg -i <src>` 的参数。ffmpeg 支持 `concat:`/`file:`/`subfile:`
#   等伪协议与 `-i` 后的额外参数注入，若放任任意字符串，攻击者可用
#   `rtsp://x` 之外的构造读取本机任意文件（如 /etc/shadow）并通过报错回显。
#   实测该注入点存在，本次修复：字符集白名单 + 长度上限 + 拒绝危险前缀。
_RTSP_RE = re.compile(r"^rtsp://[A-Za-z0-9_.:@/?&=%~\-\[\]]{1,400}$")
_RTSP_BAD = ("concat:", "subfile:", "file:", "pipe:", "crypto:", "data:", "async:", "\n", "\r", "\x00")


def validate_rtsp(src):
    """校验 RTSP 地址；合法返回 (True, 规范化地址)，非法返回 (False, 原因)。"""
    src = (src or "").strip()
    if len(src) > 400:
        return False, "RTSP 地址过长(>400)"
    low = src.lower()
    for bad in _RTSP_BAD:
        if bad in low:
            return False, "RTSP 地址含非法序列 %r" % bad
    if not _RTSP_RE.match(src):
        return False, "RTSP 地址格式非法(仅允许 rtsp:// 主机/路径 与常见参数)"
    # 主机段不能为空
    host = src[len("rtsp://"):].split("/")[0].split("@")[-1].split(":")[0]
    if not host or host in ("localhost", "127.0.0.1", "0.0.0.0", "::1"):
        return False, "RTSP 主机不允许指向本机"
    return True, src


# 工作时间 HH:MM（00:00 ~ 23:59）
_HM_RE = re.compile(r"^([01][0-9]|2[0-3]):[0-5][0-9]$")


def _finite(v, name):
    """float 转换并要求有限值，非有限(nan/inf)抛 ValueError。

    ★为什么要专门拦(2026-10-22 第三轮审查): NaN/Inf 与任何数比较恒为 False，
    会穿透 validate 里全部范围/正负校验；json.dump 默认 allow_nan=True 又把
    字面量 NaN 写进 config.json —— 主服务 serde_json 拒读 → announce-rs
    启动即崩循环，且 Python json.load 宽容读回，页面上看不出任何异常。
    """
    f = float(v)
    if not math.isfinite(f):
        raise ValueError("%s 必须是有限数字（不能为 nan/inf）" % name)
    return f


def validate_and_build(body):
    """从表单 dict 构造合法 config dict, 有问题返回 (None, 错误信息)。"""
    try:
        src_type = body["src_type"]
        if src_type == "rtsp":
            rtsp = str(body["rtsp"]).strip()
            ok, rtsp_or_msg = validate_rtsp(rtsp)
            if not ok:
                return None, "RTSP 地址不合法: " + rtsp_or_msg
            camera_source = rtsp_or_msg
        elif src_type == "usb":
            camera_source = str(int(body["usb_idx"]))
        elif src_type == "simulate":
            camera_source = "simulate"
        else:
            return None, "未知源类型: %r" % src_type

        w, h = int(body["capture_width"]), int(body["capture_height"])
        if not (320 <= w <= 3840 and 240 <= h <= 2160):
            return None, "分辨率超出合理范围"
        roi = [int(x) for x in body["roi"]]
        if len(roi) != 4 or any(v < 0 for v in roi):
            return None, "ROI 必须是 4 个非负整数"
        rx, ry, rw, rh = roi
        if rw <= 0 or rh <= 0 or rx + rw > w or ry + rh > h:
            return None, "ROI 越界: (%d,%d,%d,%d) 超出 %dx%d" % (rx, ry, rw, rh, w, h)

        cur = load_cfg()   # 页面不编辑的字段全部沿用现有配置, 避免"改个阈值把模板清空"
        cur_det = cur.get("detect") or {}
        cur_tts = cur.get("tts") or {}
        cur_ocr = cur.get("ocr") or {}

        det = body["detect"]
        d = dict(cur_det)
        d.update({
            "diff_threshold": _finite(det["diff_threshold"], "diff_threshold"),
            "changed_ratio": _finite(det["changed_ratio"], "changed_ratio"),
            "fallback_ocr_sec": _finite(det["fallback_ocr_sec"], "fallback_ocr_sec"),
            "stable_frames": int(det["stable_frames"]),
            "confirm_ttl_sec": _finite(det["confirm_ttl_sec"], "confirm_ttl_sec"),
            "active_sec": _finite(det["active_sec"], "active_sec"),
        })
        # 稳帧再认静置时长: 新增字段, 页面拿不到值时沿用现有配置/默认 0.5,
        # 并钳到 [0.1, 3.0] —— 0 等于关掉稳帧(退回"一动就抓过渡帧"的旧行为)。
        try:
            sv = float(det.get("settle_sec"))
        except (TypeError, ValueError):
            sv = float(cur_det.get("settle_sec") or 0.5)
        else:
            if not math.isfinite(sv):
                return None, "settle_sec 必须是有限数字（不能为 nan/inf）"
        d["settle_sec"] = min(3.0, max(0.1, sv))
        # 号码闸门(2026-10-21): 新号相对上次播报号的最大跳幅。
        # 现场编号严格 +1 递增, 2 足以吸收"漏叫一个号"; 填 0 关闭闸门。
        try:
            mj = int(det.get("max_jump"))
        except (TypeError, ValueError):
            mj = int(cur_det.get("max_jump") or 2)
        d["max_jump"] = min(20, max(0, mj))
        # 多行列表(2026-10-21): rows=识别行数, row_pitch=相邻行纵向间距(像素)。
        # 只改行数不改行距没有意义(会把同一行读 N 遍), 所以行距为 0 时行数强制回 1。
        try:
            rw_n = int(det.get("rows"))
        except (TypeError, ValueError):
            rw_n = int(cur_det.get("rows") or 1)
        try:
            rw_p = int(det.get("row_pitch"))
        except (TypeError, ValueError):
            rw_p = int(cur_det.get("row_pitch") or 0)
        d["row_pitch"] = min(400, max(0, rw_p))
        d["rows"] = 1 if d["row_pitch"] == 0 else min(5, max(1, rw_n))
        d.setdefault("min_interval_sec", 0.5)
        if not (0 < d["changed_ratio"] < 1):
            return None, "changed_ratio 必须在 (0,1)"
        # 三个"时段类"参数必须为正 —— 页面输入框留空会被 +'' 算成 0,
        # 而 0 对 fallback_ocr_sec 意味着「关掉静止兜底重认」(源码 due_fallback 判 >0),
        # 会让"只认到号码没认到窗口"的那一轮永不重读, 现场表现为只播半句话。
        if d["fallback_ocr_sec"] <= 0:
            d["fallback_ocr_sec"] = 3.0
        if d["active_sec"] <= 0:
            d["active_sec"] = 3.0
        if d["confirm_ttl_sec"] <= 0:
            d["confirm_ttl_sec"] = 4.0

        tt = body["tts"]
        t = dict(cur_tts)
        t.update({
            "enabled": bool(tt["enabled"]),
            "repeat": max(1, int(tt["repeat"])),
            "rate": int(tt["rate"]),
            "spell_number": bool(tt.get("spell_number", False)),
            "speak_window_letter": bool(tt.get("speak_window_letter", False)),
        })
        # 播放设备(2026-10-22): 页面上送就落盘, 留空回落 None(默认设备)。
        # 漏合并的话页面上改了设备也存不进去 —— 保存"成功"但配置纹丝不动。
        t["aplay_device"] = str(tt.get("aplay_device") or "").strip() or None
        if tt.get("piper_length_scale") is not None:
            try:
                ls = float(tt["piper_length_scale"])
            except (TypeError, ValueError):
                return None, "语速必须是数字（如 1.25）"
            if not math.isfinite(ls) or not (0.5 <= ls <= 3.0):
                return None, "语速需在 0.5 ~ 3.0 之间（1.0=常速，越大越慢）"
            t["piper_length_scale"] = round(ls, 2)
        t.setdefault("engine", "auto")
        t.setdefault("template", "请{number}到{window}号窗口办理")
        t.setdefault("gap_sec", 0.35)
        if "aplay_device" not in t:
            t["aplay_device"] = None

        oc = body["ocr"]
        o = dict(cur_ocr)
        o.update({
            "bin": "tesseract",
            "lang": str(oc.get("lang", cur_ocr.get("lang", "chi_sim"))),
            "psm": int(oc["psm"]),
        })
        o.setdefault("dpi", 150)
        # pattern(正则模板)刻意不在页面上编辑(改错会漏播): 页面未上送时沿用现有配置
        pat = oc.get("pattern", cur_ocr.get("pattern"))
        if not isinstance(pat, list) or not pat:
            return None, "ocr.pattern 缺失：该字段页面不可编辑，请先在 config.json 中配置正则模板"
        o["pattern"] = pat

        # 工作时间调度（页面可编辑）。存成 schedule 段；真正生效由
        # announce-schedule-apply 生成 systemd timer 完成，主程序不读该字段。
        sc = body.get("schedule") or {}
        sched = dict(cur.get("schedule") or {})
        sched["enabled"] = bool(sc.get("enabled"))
        ws = str(sc.get("work_start") or sched.get("work_start") or "08:00").strip()
        we = str(sc.get("work_end") or sched.get("work_end") or "18:00").strip()
        if not _HM_RE.match(ws) or not _HM_RE.match(we):
            return None, "工作时间格式非法（需 HH:MM，如 08:00 / 18:00）"
        if ws == we:
            return None, "工作开始时间与结束时间不能相同"
        sched["work_start"], sched["work_end"] = ws, we

        cfg = cur
        cfg.update({
            "camera_source": camera_source,
            "capture_width": w, "capture_height": h,
            "fps": _finite(body["fps"], "fps"),
            "roi": roi,
            "detect": d, "tts": t, "ocr": o,
            "schedule": sched,
        })
        return cfg, None
    except (KeyError, TypeError, ValueError) as e:
        return None, "参数缺失或非法: %s" % e


# ---------------------------------------------------------------- HTML

PAGE = r"""<!doctype html><html lang=zh><head><meta charset=utf-8>
<meta name=viewport content="width=device-width,initial-scale=1">
<title>叫号播报 · 配置台</title>
<style>
body{font-family:system-ui,"Microsoft YaHei",sans-serif;margin:16px;background:#f5f6f8;color:#222}
h1{font-size:20px;margin:0 0 8px}
fieldset{background:#fff;border:1px solid #ddd;border-radius:8px;margin-bottom:12px;padding:10px 14px}
legend{font-weight:600;font-size:14px}
label{display:inline-block;margin:4px 10px 4px 0;font-size:13px}
input,select{padding:4px 6px;border:1px solid #bbb;border-radius:4px;font-size:13px}
input[type=text],input[type=number]{width:90px}
#rtsp{width:420px}
button{padding:6px 16px;border:0;border-radius:6px;background:#2563eb;color:#fff;font-size:14px;cursor:pointer}
button:hover{background:#1d4ed8}
button.gray{background:#6b7280}
#pv{border:1px solid #999;cursor:crosshair;display:block;max-width:100%}
#msg{white-space:pre-wrap;background:#111;color:#9f9;padding:8px;border-radius:6px;min-height:20px;font-size:12px}
.row{margin:6px 0}
.small{font-size:12px;color:#666}
#roi4 input{width:64px}
table.log{font-size:11px;border-collapse:collapse;width:100%}
table.log td,table.log th{border:1px solid #e2e2e2;padding:2px 6px}
</style></head><body>
<h1>叫号播报 · 配置台 <span id=st class=small></span></h1>

<fieldset><legend>服务器时间</legend>
<div class=row>服务器时间 <b id=srvtime style="font-size:17px;font-variant-numeric:tabular-nums">--</b>
 <span class=small id=srvinfo></span>
 <button class="gray" type=button onclick=loadTime() style="margin-left:6px;padding:2px 10px;font-size:12px">↻</button></div>
<div class=row>手工校时 <input id=setdt type=text placeholder="2026-10-22 12:30:00" style="width:175px">
 <button type=button onclick=setTime()>写入</button>
 <button class=gray type=button onclick=useBrowserTime()>用本机时间校准</button>
 <span class=small id=timeres></span></div>
<div class=small>本机无外网，不会自动对时。时间偏差会影响播报日志时间与"每日零点清去重表"，建议在无人办事时段校时（改完立即写 RTC，重启不回退）。</div>
</fieldset>

<fieldset><legend>摄像头</legend>
<div class=row>源类型:
<select id=src_type onchange="srcUI()">
 <option value=simulate>simulate（测试帧）</option>
 <option value=usb>USB 摄像头</option>
 <option value=rtsp>RTSP 网络摄像头</option>
</select>
USB 设备 <select id=usb_dev onchange="if(this.value>=0)$('usb_idx').value=this.value">
 <option value=-1>（点「自动发现」）</option></select>
<button class=gray type=button onclick="scanUsb()">🔍 自动发现</button>
<input id=usb_idx type=number value=0 min=0 style="width:64px" title="手动设备号（下拉无果时用）">
<span id=usbinfo class=small></span>
分辨率 <input id=cw type=number value=1920> × <input id=ch type=number value=1080>
fps <input id=fps type=number value=3 step=0.5>
</div>
<div class=row>RTSP 地址（含 IP、账号、密码）:<br>
<input id=rtsp type=text size=64 placeholder="rtsp://admin:密码@192.168.1.64:554/h264/ch1/main/av_stream">
</div>
<div class=small>海康: /h264/ch1/main/av_stream · 大华: /h264/ch1 · 密码含 @:/: 等符号需 URL 转义</div>
<div class=row><button onclick=snap()>抓帧预览</button>
<label class=small style="margin-left:10px"><input type=checkbox id=live onchange=toggleLive()> 实时预览</label>
<select id=livegap onchange="if($('live').checked)toggleLive()">
<option value=2>2 秒/帧</option><option value=5 selected>5 秒/帧</option><option value=10>10 秒/帧</option>
</select>
<span class=small id=livetip>默认关闭 —— 实时预览会持续占用小主机 CPU；只看一眼用「抓帧预览」即可</span></div>
</fieldset>

<fieldset><legend>识别框 ROI —— 在预览图上按住鼠标拖一个框（框住“当前办理”文字行）</legend>
<canvas id=pv width=960 height=540></canvas>
<div class=row id=roi4>X <input id=rx type=number> Y <input id=ry type=number>
宽 <input id=rw type=number> 高 <input id=rh type=number>
<button class=gray onclick=clearRoi()>清除</button>
<button class=gray type=button onclick=testOcr()>🩺 测试识别</button>
<b id=ocrres class=small></b></div>
<div class=small>没抓帧时也可直接填坐标（0,0 起）。<b>ROI 只框住“当前办理”那一行号码</b>（约 800×120 量级）；
框到整屏（近 200 万像素）会让识别从 1 秒变成 30 秒，播报严重滞后。ROI 避开滚动字幕行。</div>
</fieldset>

<fieldset><legend>检测参数</legend>
<label>帧差阈值 diff_threshold <input id=d_diff type=number step=1></label>
<label>变化占比 changed_ratio <input id=d_ratio type=number step=0.0005></label>
<label>确认轮数 stable_frames <input id=d_stable type=number min=1 max=5></label>
<label>确认有效期 confirm_ttl_sec <input id=d_ttl type=number step=0.5 min=1></label>
<label>观察期 active_sec <input id=d_act type=number step=0.5 min=0.5></label>
<label>兜底OCR间隔 fallback_ocr_sec <input id=d_fb type=number step=0.5 min=0.5></label>
<label>稳帧再认 settle_sec <input id=d_settle type=number step=0.1 min=0.1></label>
<label>跳号上限 max_jump <input id=d_jump type=number step=1 min=0 max=20>（0=关闭；2 表示只放行 +1/+2）</label>
<label>列表行数 rows <input id=d_rows type=number step=1 min=1 max=5>（1=只认框内这一行）</label>
<label>行距 row_pitch <input id=d_pitch type=number step=1 min=0 max=400>（像素，相邻两行间距；0=不分行）</label>
</fieldset>

<fieldset><legend>播报</legend>
<label><input id=t_en type=checkbox> 启用语音（现场接线后才开）</label>
<label>次数 repeat <input id=t_rep type=number min=1 max=5></label>
<label>语速（piper，1.0=常速，越大越慢）<input id=t_len type=number step=0.05 min=0.6 max=2.0></label>
<label>espeak 语速 rate <input id=t_rate type=number min=80 max=250>（仅 espeak 引擎生效）</label>
<label>播放设备 ALSA <input id=t_dev type=text placeholder="留空=系统默认" style="width:150px">
<span class=small>（换 USB 声卡时填 <code>plughw:CARD=Device,DEV=0</code> 或 <code>plughw:1,0</code>）</span></label>
<label><input id=t_spell type=checkbox> 号码逐字念（GA204→G A 二零四）</label>
<label><input id=t_wl type=checkbox> 念窗口字母（C04→西 零 四；<b>默认不勾=不念字母</b>，只念"零 四"）</label>
<div class=row><button class=gray type=button onclick=testTts()>🔊 测试播报</button>
<button class=gray type=button onclick=fixAudio()>🔧 音量修复</button>
<span id=audioinfo class=small>读音量…</span></div>
<div class=small>「测试播报」用<b>上面填写的次数/语速/逐字念</b>立刻念一句样例（GA200→08号窗口），
<b>不用先保存</b>也能试听（试听不会改动配置，正式生效仍需点「保存」）；
<b>没声音先点「音量修复」</b>（现场九成是声卡 Master 被静音或归零）。</div>
</fieldset>

<fieldset><legend>工作时间调度（到点自动启停播报服务）</legend>
<label><input id=s_en type=checkbox onchange=schedInfo()> 启用工作时间调度</label>
<label>开始 <input id=s_start type=time value="08:00" onchange=schedInfo()></label>
<label>结束 <input id=s_end type=time value="18:00" onchange=schedInfo()></label>
<span id=sinfo class=small>…</span>
<div class=small>开启后由系统按时自动<b>拉起/停止播报服务</b>：非工作时间不采集、不识别、不播报
（省 CPU，也避免非营业时段误播）。保存后立即生效；跨午夜时段（如 22:00 ~ 06:00）同样支持。</div>
</fieldset>

<fieldset><legend>OCR</legend>
<label>语言 <input id=o_lang type=text value=chi_sim size=10></label>
<label>psm <input id=o_psm type=number min=3 max=13></label>
<span class=small>正则模板 <b id=patinfo>…</b>（页面不可编辑，保存时自动沿用现有配置）</span>
</fieldset>

<fieldset><legend>主机网络设置（改错有 90 秒自动回滚保护，不会变砖）</legend>
<div class=row>当前: <b id=cur_ip>…</b> 网关: <b id=cur_gw>…</b>
<span id=netpend style="color:#c00"></span></div>
<div class=row>新 IP <input id=n_ip type=text size=14 placeholder="192.168.31.3">
掩码 <select id=n_prefix><option value=24>/24 (255.255.255.0)</option>
<option value=16>/16</option><option value=8>/8</option><option value=25>/25</option>
<option value=26>/26</option><option value=28>/28</option><option value=32>/32</option></select>
网关 <input id=n_gw type=text size=14>（直连调试可留空）
DNS <input id=n_dns type=text size=24>（可留空）</div>
<div class=row>
<button onclick=applyNet()>应用网络设置</button>
<button class=gray onclick=factoryNet()>恢复出厂 IP (192.168.31.1)</button>
<button id=confirmbtn style="display:none;background:#c02626"
 onclick=confirmNet()>✔ 确认保留新网络配置</button>
</div>
<div class=small style="color:#c00">⚠ 192.168.31.1 在多数现场是路由器地址，有路由器时勿用（会冲突掉线）；出厂 IP 仅供网线直连调试场景。</div>
<div class=small>应用后页面会断开属正常：90 秒内用<b>新 IP</b> 重新打开本页并点「确认保留」，否则自动回滚旧 IP。</div>
</fieldset>

<button onclick=save()>💾 保存并重启播报服务</button>
<pre id=msg></pre>

<fieldset><legend>播报日志尾部（announce_log.csv）</legend>
<div id=logbox class=small>加载中…</div></fieldset>

<fieldset><legend>日志与磁盘（每日 03:30 自动清理）</legend>
<div id=loginfo class=small>加载中…</div>
<div class=row>
<button class=gray type=button onclick=logAct('archive_csv')>📦 归档并清空播报日志</button>
<button class=gray type=button onclick=logAct('clear_trig')>🧹 清理存证截图</button>
<button class=gray type=button onclick=loadLogs()>🔄 刷新</button>
<span id=logmsg class=small></span>
</div>
<div class=small>自动清理只动本目录内的白名单文件（announce_log.csv / trig_*.jpg / logs_archive/ / *.bak.*）：
播报日志超 8MB 才轮转（保留最近 6 个归档）、存证截图留 7 天、部署备份留 30 天、journal 封顶 64MB；
磁盘可用低于 500MB 时自动激进清理。归档历史<b>不会被自动删除</b>。</div>
</fieldset>

<script>
const $=id=>document.getElementById(id);

// ---- 服务器时间面板 ----
// 显示用"服务器 epoch + 本地单调时钟"推算(不每秒打请求); 时区偏移用服务器给的,
// 这样即使浏览器时区不是 +8 也不会显示错。
let srvEp=0,srvAt=0,srvOff=28800,pendingId='';
function fmtT(ep,off){const d=new Date((ep+off)*1000),p=n=>String(n).padStart(2,'0');
 return d.getUTCFullYear()+'-'+p(d.getUTCMonth()+1)+'-'+p(d.getUTCDate())+' '
  +p(d.getUTCHours())+':'+p(d.getUTCMinutes())+':'+p(d.getUTCSeconds());}
function tickSrv(){if(!srvEp)return;
 $('srvtime').textContent=fmtT(srvEp+(performance.now()-srvAt)/1000,srvOff);}
setInterval(tickSrv,500);
function loadTime(){fetch('/api/time').then(r=>r.json()).then(d=>{
 srvEp=d.epoch;srvAt=performance.now();srvOff=d.offset||0;tickSrv();
 let s='时区 '+(d.tz||'?')+' · '+(d.synced?'NTP 已同步':(d.ntp?'NTP 开启未同步':'NTP 未启用'));
 if(d.uptime)s+=' · 已运行 '+d.uptime;
 if(d.rtc_drift!=null&&Math.abs(d.rtc_drift)>2)s+=' · RTC 偏差 '+d.rtc_drift+'s';
 $('srvinfo').textContent=s;
 if(d.last)$('timeres').textContent=d.last.replace(/\s(\d{10,}-\d+)\|/,' ');
 if(pendingId&&d.last&&d.last.indexOf(pendingId)>=0)pendingId='';
}).catch(()=>{});}
function setTime(){
 const v=($('setdt').value||'').trim();
 if(!/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/.test(v)){$('timeres').textContent='格式要 2026-10-22 12:30:00';return;}
 $('timeres').textContent='提交中…';
 fetch('/api/time',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({datetime:v})})
  .then(r=>r.json()).then(d=>{
   if(!d.ok){$('timeres').textContent=d.msg||'失败';return;}
   pendingId=d.id||'';$('timeres').textContent='已提交，等待生效…';
   let n=0;const t=setInterval(()=>{n++;loadTime();
    if(!pendingId||n>10){clearInterval(t);
     if(pendingId){$('timeres').textContent='未见结果，请点 ↻ 刷新';pendingId='';}}},1000);
  }).catch(()=>{$('timeres').textContent='请求失败';});
}
function useBrowserTime(){$('setdt').value=fmtT(Date.now()/1000,srvOff);setTime();}
const pv=$('pv'),ctx=pv.getContext('2d');
let img=null,dragging=false,ds=null;

function msg(t){$('msg').textContent=t;}
function srcUI(){const u=$('src_type').value==='usb';
 $('rtsp').style.display=u?'none':'';
 $('rtsp').parentNode.style.display=$('src_type').value==='rtsp'?'':'none';
 $('usb_dev').style.display=u?'':'none';
 $('usb_idx').style.display=u?'':'none';}

function scanUsb(sel){
 fetch('/api/devices').then(r=>r.json()).then(d=>{
  const s=$('usb_dev'),ds=d.devices||[];
  s.innerHTML='';
  if(!ds.length){s.innerHTML='<option value=-1>未发现 USB 摄像头</option>';
   $('usbinfo').textContent='';return;}
  ds.forEach(v=>{
   const o=document.createElement('option');
   o.value=v.index;
   o.textContent='video'+v.index+' · '+v.name+(v.bus?' · 口 '+v.bus:'')+(v.error?' ('+v.error+')':'');
   s.appendChild(o);});
  if(sel!=null&&ds.some(v=>v.index===sel&&!v.error)){
   s.value=sel;$('usb_idx').value=sel;$('usbinfo').textContent='';}
  else if(sel==null&&ds.some(v=>!v.error)){
   const f=ds.find(v=>!v.error);s.value=f.index;$('usb_idx').value=f.index;
   $('usbinfo').textContent='已自动选中 ';}
  else{$('usb_idx').value=sel!=null?sel:$('usb_idx').value;
   $('usbinfo').textContent='所选设备无采集权限，请刷新重试';}
 }).catch(()=>{$('usbinfo').textContent='自动发现失败';});
}

function fillForm(c){
 const s=c.camera_source;
 if(s==='simulate'){$('src_type').value='simulate';}
 else if(/^[0-9]+$/.test(s)){$('src_type').value='usb';$('usb_idx').value=s;scanUsb(parseInt(s,10));}
 else {$('src_type').value='rtsp';$('rtsp').value=s;}
 $('cw').value=c.capture_width;$('ch').value=c.capture_height;$('fps').value=c.fps;
 $('rx').value=c.roi[0];$('ry').value=c.roi[1];$('rw').value=c.roi[2];$('rh').value=c.roi[3];
 $('d_diff').value=c.detect.diff_threshold;$('d_ratio').value=c.detect.changed_ratio;
 $('d_stable').value=c.detect.stable_frames;$('d_ttl').value=c.detect.confirm_ttl_sec;
 $('d_act').value=c.detect.active_sec;$('d_fb').value=c.detect.fallback_ocr_sec;
 $('d_settle').value=(c.detect.settle_sec!=null?c.detect.settle_sec:0.5);
 $('d_jump').value=(c.detect.max_jump!=null?c.detect.max_jump:2);
 $('d_rows').value=(c.detect.rows!=null?c.detect.rows:1);
 $('d_pitch').value=(c.detect.row_pitch!=null?c.detect.row_pitch:0);
 $('t_en').checked=c.tts.enabled;$('t_rep').value=c.tts.repeat;$('t_rate').value=c.tts.rate;
 $('t_len').value=(c.tts.piper_length_scale!=null?c.tts.piper_length_scale:1.25);
 $('t_wl').checked=c.tts.speak_window_letter||false;
 $('t_spell').checked=c.tts.spell_number||false;
 $('t_dev').value=c.tts.aplay_device||'';
 const sc=c.schedule||{};
 $('s_en').checked=!!sc.enabled;
 $('s_start').value=sc.work_start||'08:00';
 $('s_end').value=sc.work_end||'18:00';
 $('o_lang').value=c.ocr.lang;$('o_psm').value=c.ocr.psm;
 const pn=(c.ocr.pattern||[]).length;
 $('patinfo').textContent=pn?pn+' 条':'0 条（保存会被拒绝）';
 $('patinfo').style.color=pn?'':'#c00';
 srcUI();
 schedInfo();
}

// 工作时间状态提示：用浏览器本地时间判断当前是否在窗口内（跨午夜亦支持）。
function schedInfo(){
 const en=$('s_en').checked,s=$('s_start').value,e=$('s_end').value,el=$('sinfo');
 if(!en){el.textContent='未启用 —— 播报服务全天运行';el.style.color='';return;}
 if(!s||!e){el.textContent='请填写开始与结束时间';el.style.color='#c00';return;}
 const tm=t=>{const p=t.split(':');return (+p[0])*60+(+p[1]||0);};
 const a=tm(s),b=tm(e),now=new Date(),n=now.getHours()*60+now.getMinutes();
 if(a===b){el.textContent='开始与结束时间相同，保存会被拒绝';el.style.color='#c00';return;}
 const inw=(a<b)?(n>=a&&n<b):(n>=a||n<b);
 el.textContent=inw?'当前 处于工作时间内 —— 服务应运行中':'当前 处于非工作时间 —— 服务应已停止';
 el.style.color=inw?'#0a7':'#c00';
}

function collect(){
 return {src_type:$('src_type').value,usb_idx:$('usb_idx').value,rtsp:$('rtsp').value,
  capture_width:+$('cw').value,capture_height:+$('ch').value,fps:+$('fps').value,
  roi:[+$('rx').value||0,+$('ry').value||0,+$('rw').value||0,+$('rh').value||0],
  detect:{diff_threshold:+$('d_diff').value,changed_ratio:+$('d_ratio').value,
   stable_frames:+$('d_stable').value,confirm_ttl_sec:+$('d_ttl').value,
   active_sec:+$('d_act').value,fallback_ocr_sec:+$('d_fb').value,
   settle_sec:+$('d_settle').value,max_jump:+$('d_jump').value,
   rows:+$('d_rows').value,row_pitch:+$('d_pitch').value},
  tts:{enabled:$('t_en').checked,repeat:+$('t_rep').value,rate:+$('t_rate').value,
   piper_length_scale:+$('t_len').value,speak_window_letter:$('t_wl').checked,
   spell_number:$('t_spell').checked,
   aplay_device:$('t_dev').value.trim()||null},
  ocr:{lang:$('o_lang').value,psm:+$('o_psm').value},
  schedule:{enabled:$('s_en').checked,work_start:$('s_start').value,work_end:$('s_end').value}};
}

let liveTimer=null,snapBusy=false,prevUrl=null,autoFail=0;
// 实时预览的刷新间隔(秒): 现场为"对准摄像头/框 ROI"临时开一下, 用完关掉。
function liveGapSec(){const el=$('livegap');const v=el?parseInt(el.value,10):5;return (v>=2&&v<=60)?v:5;}
function liveTip(t){if($('livetip'))$('livetip').textContent=t;}
function liveStop(reason){
 if(liveTimer){clearInterval(liveTimer);liveTimer=null;}
 if($('live'))$('live').checked=false;
 liveTip(reason||'实时预览已关闭 —— 看画面用「抓帧预览」即可');
}
function toggleLive(){
 const el=$('live'),on=!!(el&&el.checked);
 if(liveTimer){clearInterval(liveTimer);liveTimer=null;}
 if(!on){if(el)liveTip('实时预览已关闭 —— 看画面用「抓帧预览」即可');return;}
 if(document.hidden){liveStop('页面在后台，未开启（切到前台再勾）');return;}
 const sec=liveGapSec();
 msg('实时预览已开启（'+sec+' 秒 1 帧，拖动 ROI 时自动暂停）');
 snap();
 liveTimer=setInterval(()=>snap(true),sec*1000);
 liveTip('实时中：'+sec+' 秒 1 帧（拖动 ROI 时自动暂停；关闭页面/切后台即停）');
}
// 页面切到后台必须停 —— 否则浏览器"看不着"却在后台一直请求, 白白占小主机 CPU。
document.addEventListener('visibilitychange',()=>{
 if(document.hidden&&liveTimer)liveStop('已暂停（页面切到后台）');
});

// auto=true 为定时器调用: 静默刷新(不刷提示), 拖动 ROI 时让路, 上一次未返回则不叠请求
function snap(auto){
 const isAuto=(auto===true);
 if(snapBusy||(isAuto&&dragging))return;
 if(isAuto&&$('live')&&!$('live').checked){liveStop();return;}
 const t=$('src_type').value;
 let src=t;
 if(t==='rtsp')src=encodeURIComponent($('rtsp').value.trim());
 if(t==='usb')src=$('usb_idx').value;
 snapBusy=true;
 if(!isAuto)msg('抓帧中…');
 // ★URL 必须带时间戳: /api/snapshot 的 URL 固定不变时浏览器可能直接给缓存图,
 //   表现就是"抓帧预览抓的不是实时的"(后端其实每次都新抓, 是浏览器复用缓存)。
 fetch('/api/snapshot?src='+src+'&w='+$('cw').value+'&h='+$('ch').value+'&_t='+Date.now(),
       {cache:'no-store'})
 .then(r=>{if(!r.ok)return r.text().then(txt=>{throw new Error(txt)});return r.blob();})
 .then(b=>{if(isAuto)autoFail=0;const im=new Image(),u=URL.createObjectURL(b);
   im.onload=()=>{if(prevUrl)URL.revokeObjectURL(prevUrl);prevUrl=u;img=im;
     // ★只在尺寸变了才改 canvas 尺寸(改一次等于清一次画布), 实时预览时避免每秒整屏重绘
     const nw=Math.min(1100,im.naturalWidth),nh=Math.max(1,Math.round(nw*im.naturalHeight/im.naturalWidth));
     if(pv.width!==nw||pv.height!==nh){pv.width=nw;pv.height=nh;}
     draw();
     if(!isAuto)msg('预览就绪 ('+im.naturalWidth+'x'+im.naturalHeight+')，拖框选 ROI');};
   im.onerror=()=>{URL.revokeObjectURL(u);if(!isAuto)msg('图像解码失败');};
   im.src=u;})
 .catch(e=>{
   if(!isAuto){msg('抓帧失败: '+e.message);return;}
   // 实时预览连续失败(如非工作时间服务已停机)就自动关掉, 不要一直空转刷日志
   autoFail++;
   if(autoFail>=3&&$('live')){liveStop('实时预览已停止：连续 3 次抓帧失败（'+e.message+'）');}})
 .finally(()=>{snapBusy=false;});
}

// ROI 数值一律以「采集分辨率」为准(= 页面上的 分辨率 cw×ch, 也是主程序抓帧的尺寸)。
// ★预览图现在由小主机侧缩过图(默认 960×540), 所以换算必须用 cw 而非 img.naturalWidth,
//   否则拖出来的 ROI 会只有真实坐标的一半, 主程序裁错区域、识别必然失败。
function capW(){const el=$('cw');const v=el?parseFloat(el.value):0;return v>0?v:(img?img.naturalWidth:1);}
function draw(){
 if(!img)return;
 ctx.drawImage(img,0,0,pv.width,pv.height);
 const s=pv.width/capW();
 const r=[+$('rx').value||0,+$('ry').value||0,+$('rw').value||0,+$('rh').value||0];
 ctx.strokeStyle='#00e05a';ctx.lineWidth=2;ctx.strokeRect(r[0]*s,r[1]*s,r[2]*s,r[3]*s);
 ctx.fillStyle='#00e05a';ctx.font='12px sans-serif';
 ctx.fillText('ROI',r[0]*s+4,r[1]*s+14);
}

function clearRoi(){$('rx').value=0;$('ry').value=0;$('rw').value=0;$('rh').value=0;draw();}

pv.addEventListener('mousedown',e=>{if(!img)return;dragging=true;ds=mousePos(e);});
pv.addEventListener('mousemove',e=>{if(!dragging)return;const p=mousePos(e);setRoi(ds,p);draw();});
window.addEventListener('mouseup',()=>{dragging=false;});
function mousePos(e){const r=pv.getBoundingClientRect(),s=capW()/pv.width;
 return {x:(e.clientX-r.left)*s,y:(e.clientY-r.top)*s};}
function setRoi(a,b){
 const x=Math.min(a.x,b.x),y=Math.min(a.y,b.y);
 $('rx').value=Math.round(x);$('ry').value=Math.round(y);
 $('rw').value=Math.round(Math.abs(a.x-b.x));$('rh').value=Math.round(Math.abs(a.y-b.y));
}

function save(){
 if(!confirm('保存配置并重启 announce-rs？'))return;
 msg('保存中…');
 // ★乐观锁: 带上页面加载/上次保存时的配置指纹, 期间被别的窗口改过就 409 拒保存,
 //   防止"旧快照整份覆盖他人新改动"。409 时按提示刷新重载即可。
 const body=collect(); body.__rev=cfgRev;
 fetch('/api/save',{method:'POST',headers:{'Content-Type':'application/json'},
  body:JSON.stringify(body)})
 .then(r=>r.json().then(d=>({ok:r.ok,d})))
 .then(({ok,d})=>{if(d.rev)cfgRev=d.rev;msg(d.error||d.msg||JSON.stringify(d));if(ok)refresh();})
 .catch(e=>msg('保存失败: '+e));
}

function refresh(){
 fetch('/api/status').then(r=>r.json()).then(d=>{
  $('st').textContent='· 播报服务: '+d.active+' · PID '+d.pid+' · '+d.mem;
  $('logbox').textContent=d.log||'(暂无日志)';
 }).catch(()=>{});
 loadLogs();
}

function loadNet(){
 fetch('/api/network').then(r=>r.json()).then(d=>{
  $('cur_ip').textContent=d.ip?(d.ip+' /'+d.prefix):'(未知)';
  $('cur_gw').textContent=d.gw||'无';
  $('n_ip').value=d.ip;$('n_prefix').value=d.prefix||24;$('n_gw').value=d.gw||'';
  $('confirmbtn').style.display=d.pending?'':'none';
  $('netpend').textContent=d.pending?'⚠ 有修改待确认(90秒内不确认将回滚)':'';
 }).catch(()=>{});
}
function showLogs(d){
 if(!d)return;
 const kb=v=>(v/1024).toFixed(1)+' KB';
 const dk=d.disk||{};
 let s='';
 s+='播报日志 announce_log.csv：'+kb(d.csv_bytes||0)+'，<b>'+(d.csv_lines||0)+'</b> 条';
 if(d.csv_first)s+='　<span style="color:#666">'+d.csv_first+' ~ '+d.csv_last+'</span>';
 s+='<br>存证截图：<b>'+(d.trig_count||0)+'</b> 张，'+kb(d.trig_bytes||0)+'（主程序自身上限 20 张，超出会覆盖）';
 s+='<br>日志归档：<b>'+(d.archive_count||0)+'</b> 个';
 (d.archive||[]).slice(0,4).forEach(a=>{s+='　· '+a.name+' ('+a.kb+' KB)';});
 s+='<br>systemd journal：<b>'+(d.journal||'?')+'</b>（上限 64MB，超出自动丢弃最旧的）';
 s+='　磁盘：已用 '+(dk.used_mb||0)+'/'+(dk.total_mb||0)+' MB（<b>'+(dk.pct||0)+'%</b>），可用 '+(dk.avail_mb||0)+' MB';
 s+='<br>自动清理：'+(d.timer_enabled==='enabled'
      ? '<b style="color:#060">已启用</b>（每日 03:30：轮转日志、清理过期存证、收缩 journal）'
      : '<b style="color:#c00">未启用</b>');
 if(d.timer_next)s+='，下次 '+d.timer_next;
 $('loginfo').innerHTML=s;
}
function loadLogs(){fetch('/api/logs').then(r=>r.json()).then(showLogs).catch(()=>{});}
function logAct(a){
 if(a==='archive_csv'&&!confirm('把当前播报日志归档到 logs_archive/ 并清空？\\n\\n归档文件会保留（最近 6 个），不会丢历史。'))return;
 $('logmsg').textContent='执行中…';
 fetch('/api/logs',{method:'POST',headers:{'Content-Type':'application/json'},
  body:JSON.stringify({action:a})})
 .then(r=>r.json()).then(d=>{$('logmsg').textContent=d.msg||'';
   if(d.logs)showLogs(d.logs);refresh();})
 .catch(e=>{$('logmsg').textContent='失败: '+e.message;});
}

function applyNet(){
 const ip=$('n_ip').value.trim();
 if(!/^[0-9.]+$/.test(ip)){msg('IP 格式非法');return;}
 if(!confirm('把主机 IP 改为 '+ip+' ?\\n\\n应用后本页面将断开，90 秒内请用新 IP 重开本页并点「确认保留」，否则自动回滚。'))return;
 msg('应用网络配置中…页面即将断开');
 fetch('/api/network',{method:'POST',headers:{'Content-Type':'application/json'},
  body:JSON.stringify({ip:ip,prefix:+$('n_prefix').value,gw:$('n_gw').value.trim(),dns:$('n_dns').value.trim()})})
 .then(r=>r.json()).then(d=>{
   if(d.ok){msg('已应用: '+d.msg+'\\n→ 请改用 http://'+ip+':8080 重新打开本页并点「确认保留」');}
   else msg('应用失败: '+d.error);
 }).catch(()=>msg('连接已断开（正常现象）——若未成功请等 90 秒自动回滚后重试；成功则用新 IP 访问'));
}
function confirmNet(){
 fetch('/api/network/confirm',{method:'POST'}).then(r=>r.json()).then(d=>{
  msg(d.msg);loadNet();}).catch(e=>msg('confirm 失败: '+e));
}
function factoryNet(){
 if(!confirm('恢复出厂 IP 192.168.31.1 ?\\n\\n注意: 该地址在有路由器的现场通常被占用！仅网线直连调试时使用。\\n应用后 90 秒内需用新地址确认。'))return;
 msg('恢复出厂 IP 中…页面即将断开');
 fetch('/api/network',{method:'POST',headers:{'Content-Type':'application/json'},body:'{"factory":true}'})
 .then(r=>r.json()).then(d=>{
   if(d.ok)msg('已应用出厂 IP\\n→ 请改用 http://192.168.31.1:8080 重开本页并点「确认保留」');
   else msg('应用失败: '+d.error);
 }).catch(()=>msg('连接已断开（正常现象）——90 秒内用 http://192.168.31.1:8080 确认，否则自动回滚'));
}

function testOcr(){
 msg('正在抓帧并按当前 ROI 识别…（ROI 大时会比较慢，请耐心等）');
 $('ocrres').textContent='识别中…';$('ocrres').style.color='#666';
 fetch('/api/test_ocr',{method:'POST'}).then(r=>r.json()).then(d=>{
  if(!d.ok){msg('✘ '+d.msg);$('ocrres').textContent='✘ 失败';$('ocrres').style.color='#c00';return;}
  const c=(d.candidates||[]).map(x=>x.number+'→'+x.window).join(' , ');
  msg('识别耗时 '+d.elapsed+' 秒｜ROI '+d.roi.join(',')+'（'+Math.round(d.pixels/1000)+'k 像素）\n'
      +'OCR 读到：'+(d.raw||'(空)')+'\n命中号码：'+(c||'(无)'));
  const big=d.pixels>400000;
  $('ocrres').textContent=(d.elapsed>5||big)
    ? ('⚠ '+d.elapsed+'s，ROI 偏大（'+Math.round(d.pixels/1000)+'k 像素），建议缩到只框号码行')
    : ('✓ '+d.elapsed+'s'+(c?(' 命中 '+c):''));
  $('ocrres').style.color=(d.elapsed>5||big)?'#c00':'#080';
 }).catch(e=>{msg('测试识别失败: '+e);$('ocrres').textContent='';});
}

function testTts(){
 // ★带上页面"当前填写"的播报参数（不必先保存就能试听）：
 //   之前发空 body，服务端只读磁盘上已保存的 config.json，
 //   改了次数没保存直接点测试 -> 听到的还是旧值，极易误判成程序 bug。
 const tts={};
 const rep=parseInt($('t_rep').value,10);   if(rep>=1) tts.repeat=rep;
 const rat=parseInt($('t_rate').value,10);  if(rat>0) tts.rate=rat;
 const len=parseFloat($('t_len').value);    if(len>=0.5&&len<=3.0) tts.piper_length_scale=len;
 tts.speak_window_letter=!!$('t_wl').checked;
 tts.spell_number=!!$('t_spell').checked;
 // 试听也走页面填的播放设备: 换 USB 声卡后要先在这里听出声再保存
 tts.aplay_device=$('t_dev').value.trim()||null;
 msg('正在合成并播放测试播报…（约 2~15 秒）');
 fetch('/api/test_tts',{method:'POST',headers:{'Content-Type':'application/json'},
  body:JSON.stringify({tts:tts})})
 .then(r=>r.json())
 .then(d=>{msg((d.ok?'✔ ':'✘ ')+(d.msg||''));showAudio(d.audio);})
 .catch(e=>msg('测试播报失败: '+e));
}
function fixAudio(){
 msg('正在解除静音并拉满音量…');
 fetch('/api/audio_fix',{method:'POST'}).then(r=>r.json())
 .then(d=>{msg((d.ok?'✔ 音量已设为 100% 并解除静音':'✘ '+d.msg));showAudio(d.audio);})
 .catch(e=>msg('音量修复失败: '+e));
}
function showAudio(a){
 if(!a)return;
 const f=k=>{const v=a[k];if(!v)return '';return k+' '+v.pct+'%'+(v.muted?'(静音!)':'');};
 const s=[f('Master'),f('PCM')].filter(Boolean).join(' · ');
 // 把系统识别到的卡号列出来: 换 USB 声卡后一眼看出它是 card 几、有没有被认成 USB
 const cs=(a.cards||[]).map(c=>'card'+c.num+(c.usb?'(USB)':'')).join(' ');
 $('audioinfo').textContent='· 当前卡'+a.card+(cs?' ['+cs+']':'')+' '+s;
 $('audioinfo').style.color=(a.Master&&a.Master.muted)?'#c00':'';
}
function loadAudio(){fetch('/api/audio').then(r=>r.json()).then(showAudio).catch(()=>{});}

let cfgRev=null; // 配置内容指纹(乐观锁), 与 /api/config 的 rev、/api/save 的 __rev 对应
fetch('/api/config').then(r=>r.json()).then(d=>{cfgRev=d.rev||null;fillForm(d);})
 .catch(e=>msg('读配置失败: '+e));
scanUsb();loadAudio();loadTime();
// 服务器时间每 30 秒与后端对齐一次(平时靠本地单调时钟推算, 不打请求)
setInterval(loadTime,30000);
refresh();loadNet();
// ★默认不进实时预览: 小主机只有 4 核赛扬, 持续刷帧会和 OCR/TTS 抢 CPU(现场反馈"开着就卡")。
//   这里只在打开页面时抓**一张静帧**(开销 = 一次缩图编码), 好让画面不为空、能直接拖 ROI;
//   要看连续画面由用户自己勾「实时预览」(还要选 2/5/10 秒间隔)。
liveTip('默认关闭 —— 实时预览会持续占用小主机 CPU；只看一眼用「抓帧预览」即可');
if(!document.hidden)setTimeout(()=>snap(true),300);
</script></body></html>"""


# ---------------------------------------------------------------- usb cameras

_VIDIOC_QUERYCAP = 0x80685600
_V4L2_CAP_VIDEO_CAPTURE = 0x00000001   # 注意: 0x04000000 是 STREAMING, 不是 CAPTURE
_V4L2_CAP_VIDEO_CAPTURE_MPLANE = 0x00001000
_V4L2_CAP_DEVICE_CAPS = 0x80000000     # 置位时以 device_caps 为准(区分多节点设备的主/从节点)


def list_usb_cams():
    """枚举 /dev/video* 中具备「视频采集」能力的节点（滤掉 v4l2 的 metadata 从节点）。
    纯 Python V4L2 QUERYCAP ioctl, 零外部依赖。无权限时给出可读提示。"""
    out = []
    for syspath in sorted(glob.glob("/sys/class/video4linux/video*")):
        try:
            idx = int(syspath.rsplit("video", 1)[-1])
        except ValueError:
            continue
        name = ""
        try:
            with open(os.path.join(syspath, "name")) as f:
                name = f.read().strip()
        except OSError:
            pass
        dev = "/dev/video%d" % idx
        try:
            fd = os.open(dev, os.O_RDONLY)
        except OSError as e:
            if e.errno == errno.EACCES:
                out.append({"index": idx, "name": name or dev,
                            "error": "无权限访问（服务账号需加入 video 组）"})
            continue  # 其他错误(如设备忙/热拔)直接跳过
        try:
            buf = bytearray(104)
            fcntl.ioctl(fd, _VIDIOC_QUERYCAP, buf)
            # driver[16]+card[32]+bus_info[32]+version[4] -> capabilities@84, device_caps@88
            caps = struct.unpack_from("I", buf, 84)[0]
            dcaps = struct.unpack_from("I", buf, 88)[0]
            bus = buf[48:80].split(b"\x00")[0].decode("utf-8", "replace")
            eff = dcaps if caps & _V4L2_CAP_DEVICE_CAPS else caps
            if eff & (_V4L2_CAP_VIDEO_CAPTURE | _V4L2_CAP_VIDEO_CAPTURE_MPLANE):
                out.append({"index": idx, "name": name or dev, "bus": bus})
        except OSError:
            pass
        finally:
            os.close(fd)
    return out


# ---------------------------------------------------------------- 音频 / 测试播报

_CN_DIGIT = {"0": "零", "1": "一", "2": "二", "3": "三", "4": "四",
             "5": "五", "6": "六", "7": "七", "8": "八", "9": "九"}
# 拉丁字母 → 中文谐音（与主程序 tts.rs 的 LETTER_CN 保持一致，两处要同步改）。
# 为什么需要：piper 中文模型 zh_CN-huayan 的音素表是 IPA 中文集、不含英语音素，
# 而 espeak-ng 会把孤立的拉丁字母按英语读音音素化（C -> si:），模型只能合成出
# 又短又弱的音 —— 现场听感就是"这个字母被吞掉了"。换成中文同音字即走中文音素，
# 发音饱满清晰（现场叫号 C08 本就念"西零八"）。
_CN_LETTER = {"C": "西"}


def spell_tokens(s):
    """号码逐字念：数字转中文、字母按中文谐音（与主程序 spell_tokens 行为一致）。"""
    return " ".join(_CN_DIGIT.get(c, _CN_LETTER.get(c.upper(), c))
                    for c in s if not c.isspace())


def spell_digits(s):
    """窗口号只念数字、字母不念（与主程序 spell_window(s, false) 行为一致）。"""
    return " ".join(_CN_DIGIT[c] for c in s if c.isdigit())


def time_status():
    """服务器时间 / 时区 / NTP 状态 / 开机时长 / 最近一次校时结果。

    现场机器无外网, systemd-timesyncd inactive —— 时间只能手工校, 所以面板要
    把"当前时间 + 是否在自动同步 + 上次校时结果"一次显示全, 避免用户重复点。
    """
    st = {"epoch": int(time.time()), "now": "", "tz": "", "offset": 0,
          "ntp": False, "synced": False, "uptime": "", "rtc": "", "last": "",
          "last_id": ""}
    try:
        st["now"] = time.strftime("%Y-%m-%d %H:%M:%S")
        st["offset"] = int(-time.timezone if not time.daylight or
                           not time.localtime().tm_isdst else -time.altzone)
    except Exception:  # noqa: BLE001
        pass
    try:
        st["tz"] = _run_quiet(["timedatectl", "show", "-p", "Timezone", "--value"]) or \
            _run_quiet(["cat", "/etc/timezone"])
        st["ntp"] = _run_quiet(["timedatectl", "show", "-p", "NTP", "--value"]) == "yes"
        st["synced"] = _run_quiet(
            ["timedatectl", "show", "-p", "NTPSynchronized", "--value"]) == "yes"
    except Exception:  # noqa: BLE001
        pass
    try:
        with open("/proc/uptime") as f:
            up = float(f.read().split()[0])
        d, rem = divmod(int(up), 86400)
        h, rem = divmod(rem, 3600)
        st["uptime"] = ("%d 天 " % d if d else "") + "%d 小时 %d 分" % (h, rem // 60)
    except Exception:  # noqa: BLE001
        pass
    try:
        # RTC 走 UTC: since_epoch 就是 epoch, 用来对照"重启后会不会回退"
        with open("/sys/class/rtc/rtc0/since_epoch") as f:
            rtc = int(f.read().strip())
        st["rtc"] = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(rtc))
        st["rtc_drift"] = rtc - st["epoch"]
    except Exception:  # noqa: BLE001
        pass
    for p, key in ((os.path.join(TIME_DIR, "result"), "last"),
                   (os.path.join(TIME_DIR, "last_req_id"), "last_id")):
        try:
            with open(p, encoding="utf-8", errors="replace") as f:
                st[key] = f.read().strip()[:200]
        except Exception:  # noqa: BLE001
            pass
    return st


def time_submit(body):
    """把校时请求交给 root 定时器执行。返回 (ok, msg, request_id)。

    只做格式校验(与 apply 脚本同口径), 真正的 date -s 由 root 侧执行;
    页面拿到 request_id 后轮询 /api/time 的 last 字段认领结果。
    """
    dt = str(body.get("datetime") or "").strip()
    if not re.match(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$", dt):
        return False, "时间格式必须是 YYYY-MM-DD HH:MM:SS", ""
    rid = "%d-%d" % (int(time.time() * 1000), threading.get_ident() % 1000)
    try:
        os.makedirs(TIME_DIR, exist_ok=True)
        tmp = os.path.join(TIME_DIR, "request.tmp")
        with open(tmp, "w", encoding="utf-8") as f:
            f.write("%s|%s\n" % (rid, dt))
        os.replace(tmp, os.path.join(TIME_DIR, "request"))
    except Exception as e:  # noqa: BLE001
        return False, "写请求失败: %s" % e, ""
    return True, "已提交，最多 5 秒生效", rid


# ALSA 卡列表文件路径（常量提出来，便于本机单测时替换）
CARDS_FILE = "/proc/asound/cards"


def alsa_card():
    """当前声卡卡号 —— 不能写死 0。

    换 USB 声卡后卡号会变（板载若在就是 1），写死 -c 0 会导致「音量修复」
    去修板载卡、状态也读错卡。优先按配置的 aplay_device 解析：
      plughw:1,0            -> "1"
      plughw:CARD=Device,DEV=0 -> 查 /proc/asound/cards 拿到该名字的卡号
    解析不出再退回 0。
    """
    dev = ""
    try:
        dev = (load_cfg() or {}).get("tts", {}).get("aplay_device") or ""
    except Exception:
        pass
    m = re.search(r"(?:plughw|hw):(\d+)", dev)
    if m:
        return m.group(1)
    m = re.search(r"CARD=([^,]+)", dev)
    if m:
        try:
            for ln in open(CARDS_FILE, encoding="utf-8", errors="replace"):
                if ("[" + m.group(1)) in ln and ln[:3].strip().isdigit():
                    return ln.split()[0]
        except OSError:
            pass
    return "0"


def alsa_cards():
    """/proc/asound/cards 摘要：让页面上能一眼看到系统把哪张卡识别成几号、是不是 USB。"""
    out = []
    try:
        for ln in open(CARDS_FILE, encoding="utf-8", errors="replace"):
            if not ln[:3].strip().isdigit():
                continue
            num = ln.split()[0]
            body = ln.split(":", 1)[1] if ":" in ln else ln
            out.append({"num": num, "name": body.strip()[:40],
                        "usb": "USB-Audio" in body or "USB" in body})
    except OSError:
        pass
    return out


def audio_status():
    """Master/PCM 音量与静音状态（现场"没声音"九成是这里被静音/归零）。"""
    card = alsa_card()
    st = {"card": card, "cards": alsa_cards()}
    for name in ("Master", "PCM"):
        try:
            p = subprocess.run(["amixer", "-c", card, "sget", name],
                               capture_output=True, timeout=5)
            txt = p.stdout.decode("utf-8", "replace")
            pcts = re.findall(r"\[(\d+)%\]", txt)
            st[name] = {"pct": pcts[0] if pcts else "?",
                        "muted": "[off]" in txt,
                        "ok": p.returncode == 0}
        except (OSError, subprocess.SubprocessError) as e:
            st[name] = {"pct": "?", "muted": False, "ok": False, "err": str(e)}
    return st


def audio_fix():
    """一键把 Master/PCM 拉满并解除静音（debian 在 audio 组即可，无需 root）。"""
    card = alsa_card()
    logs, ok = [], True
    for name in ("Master", "PCM"):
        try:
            p = subprocess.run(["amixer", "-c", card, "sset", name, "100%", "unmute"],
                               capture_output=True, timeout=10)
            ok = ok and p.returncode == 0
            logs.append("%s rc=%d" % (name, p.returncode))
        except (OSError, subprocess.SubprocessError) as e:
            ok = False
            logs.append("%s 失败: %s" % (name, e))
    return ok, "；".join(logs)


def espeak_voice():
    """中文音色: 新版 espeak-ng 用 cmn, 老版用 zh。"""
    try:
        p = subprocess.run(["espeak-ng", "--voices=cmn"], capture_output=True, timeout=10)
        if any(l.split()[1:2] == ["cmn"] for l in p.stdout.decode("utf-8", "replace").splitlines()):
            return "cmn"
    except (OSError, subprocess.SubprocessError, IndexError):
        pass
    return "zh"


def piper_via_server(url, text, wav, timeout=60):
    """走常驻合成服务（模型已驻内存，约 2~3 秒出声，而非每次重载模型的 13 秒）。"""
    if not url:
        return False
    try:
        req = urllib.request.Request(url, data=text.encode("utf-8"))
        with urllib.request.urlopen(req, timeout=timeout) as r:
            data = r.read()
    except (OSError, ValueError):
        return False
    if not data:
        return False
    try:
        with open(wav, "wb") as f:
            f.write(data)
    except OSError:
        return False
    return True


def apply_tts_preview_overrides(cfg, ov):
    """「测试播报」的试听参数合并（纯逻辑，独立成函数供单测）。

    页面允许带上当前填写的试听参数（未保存也能先听）: 只覆盖 tts 里这几个
    无副作用的字段, 其余仍以磁盘配置为准; 本函数不写盘, 试听完页面保存与否
    都不受影响。试听参数非法就整体忽略, 退回磁盘配置。
    """
    if not isinstance(ov, dict):
        return cfg  # 页面送来数组/字符串等畸形数据: 整体忽略, 不抛 500
    try:
        t = dict(cfg.get("tts") or {})
        if ov.get("repeat") is not None:
            t["repeat"] = max(1, min(5, int(ov["repeat"])))
        if ov.get("rate") is not None:
            t["rate"] = int(ov["rate"])
        if ov.get("spell_number") is not None:
            t["spell_number"] = bool(ov["spell_number"])
        if ov.get("speak_window_letter") is not None:
            t["speak_window_letter"] = bool(ov["speak_window_letter"])
        if ov.get("piper_length_scale") is not None:
            _ls = float(ov["piper_length_scale"])
            if 0.5 <= _ls <= 3.0:
                t["piper_length_scale"] = round(_ls, 2)
        # 播放设备(2026-10-22): 页面送了该字段就以其为准(空串/None 回落默认设备)。
        # 不带上的话试听永远走磁盘配置的设备, "换个设备试听"在页面上就是假的。
        if "aplay_device" in ov:
            t["aplay_device"] = str(ov.get("aplay_device") or "").strip() or None
        cfg["tts"] = t
    except (TypeError, ValueError):
        pass  # 试听参数非法就忽略, 退回磁盘配置
    return cfg


def test_tts(cfg, number="GA200", window="08"):
    """按当前配置合成并播放（不等识别触发，直接验证声音链路）。
    引擎优先级与主程序一致: 常驻服务 > piper CLI > espeak-ng。
    播放遍数遵循配置的 repeat。"""
    tts = cfg.get("tts") or {}
    tpl = tts.get("template") or "请{number}到{window}号窗口办理"
    if tts.get("spell_number"):
        num_s = spell_tokens(number)
        win_s = (spell_tokens(window) if tts.get("speak_window_letter")
                 else spell_digits(window))
    else:
        num_s, win_s = number, window
    text = tpl.replace("{number}", num_s).replace("{window}", win_s)
    times = max(1, min(5, int(tts.get("repeat") or 1)))

    # ★2026-10-23 低危修复: 固定 /tmp 名两人同时点测试会互踩(播到半截 wav)。
    # mkstemp 唯一化; 顺带清掉 60s 前的遗留(异常路径可能跳过删除, 不清会累积;
    # 只删超时旧文件, 不会碰到并发中他人正在用的)。
    for _old in glob.glob("/tmp/_wb_tts_test_*.wav"):
        try:
            if time.time() - os.path.getmtime(_old) > 60:
                os.remove(_old)
        except OSError:
            pass
    fd, wav = tempfile.mkstemp(prefix="_wb_tts_test_", suffix=".wav")
    os.close(fd)
    bin_p = tts.get("piper_bin") or ""
    model = tts.get("piper_model") or ""
    eng = tts.get("engine") or "auto"
    url = tts.get("piper_url") or ""
    engine = None

    if eng in ("piper", "auto") and piper_via_server(url, text, wav):
        engine = "piper 常驻服务"
    if engine is None and eng in ("piper", "auto") and bin_p and model \
            and os.path.exists(bin_p) and os.path.exists(model):
        try:
            cmd = [bin_p, "--model", model, "--output_file", wav]
            _ls = tts.get("piper_length_scale")
            if _ls:
                cmd += ["--length-scale", str(_ls)]   # 与主程序一致, 试听和正式播报同速
            p1 = subprocess.run(cmd, input=text.encode("utf-8"),
                                capture_output=True, timeout=180)
        except (OSError, subprocess.SubprocessError) as e:
            return False, "piper 合成失败: %s" % e, text
        if p1.returncode != 0:
            return False, "piper 退出码 %d: %s" % (
                p1.returncode, p1.stderr.decode("utf-8", "replace").strip()[:200]), text
        engine = "piper CLI（每句重载模型，较慢）"
    if engine is None:
        try:
            p1 = subprocess.run(["espeak-ng", "-v", espeak_voice(),
                                 "-s", str(int(tts.get("rate", 135))),
                                 "-w", wav, text],
                                capture_output=True, timeout=60)
        except (OSError, subprocess.SubprocessError) as e:
            return False, "espeak 合成失败: %s（靶机需装 espeak-ng）" % e, text
        if p1.returncode != 0:
            return False, "espeak 退出码 %d: %s" % (
                p1.returncode, p1.stderr.decode("utf-8", "replace").strip()[:200]), text
        engine = "espeak-ng"

    dev = tts.get("aplay_device")
    gap = float(tts.get("gap_sec") or 0.35)
    play_cmd = ["aplay"] + (["-D", dev] if dev else ["-D", "default"]) + [wav]
    for i in range(times):
        try:
            p2 = subprocess.run(play_cmd, capture_output=True, timeout=60)
        except (OSError, subprocess.SubprocessError) as e:
            return False, "播放失败: %s" % e, text
        if p2.returncode != 0:
            return False, "aplay 退出码 %d: %s" % (
                p2.returncode, p2.stderr.decode("utf-8", "replace").strip()[:200]), text
        if i + 1 < times and gap > 0:
            time.sleep(gap)
    return True, "已用 %s 念出 %d 遍：%s" % (engine, times, text), text


def usb_preview_via_service(timeout=10.0):
    """USB 设备被主程序独占时，请主程序落盘最新帧（_live/req -> _live/latest.jpg）。

    实时性要点（改版记录）：
    - 旧实现「先删 latest.jpg → 等它重新出现 → 再等大小两次采样相同」，全程要
      0.15*2 秒+，单次抓帧 0.9 秒，页面看着就"不实时"。
    - 新实现改为**指纹轮询**：请求前记下 latest.jpg 的 (mtime_ns, size)，主程序是
      tmp+rename 原子替换，落盘后这两个值必然变化，看到变化即可读，省掉固定等待。
    - 读到后用 JPEG 结束标记 (FFD9) 兜底校验：万一跑的是旧版（直接 create 写）的
      主程序，可能拿到半截文件，此时继续等下一轮，绝不把花屏图交出去。
    """
    live = os.path.join(HERE, "_live")
    req, latest = os.path.join(live, "req"), os.path.join(live, "latest.jpg")

    def fingerprint():
        try:
            s = os.stat(latest)
            return (s.st_mtime_ns, s.st_size)
        except OSError:
            return None

    try:
        os.makedirs(live, exist_ok=True)
        base = fingerprint()
        with open(req, "wb"):
            pass
    except OSError as e:
        return None, "无法创建预览请求: %s" % e

    t0 = time.time()
    while time.time() - t0 < timeout:
        key = fingerprint()
        if key and key != base and key[1] > 1024:
            try:
                with open(latest, "rb") as f:
                    data = f.read()
            except OSError as e:
                return None, "读取预览帧失败: %s" % e
            if data[-2:] == b"\xff\xd9":  # 完整 JPEG
                return data, None
            base = key  # 半截文件(旧版主程序), 等下一次落盘
        time.sleep(0.05)
    return None, ("主程序 %.0f 秒内未给出预览帧——播报服务是否在运行？"
                  "（非工作时间会自动停机）" % timeout)


def test_ocr(cfg):
    """抓当前帧 → 按配置 ROI 裁剪 → tesseract 识别并计时 → 按配置正则解析。
    现场校准 ROI 的闭环工具：一眼看到「耗时多少秒 + 读到了什么 + 命中号码」。"""
    src = norm_src(str(cfg["camera_source"]))
    w, h = int(cfg["capture_width"]), int(cfg["capture_height"])
    if src.isdigit() and announce_running():
        data, err = usb_preview_via_service(timeout=10.0)
    else:
        data, err = ffmpeg_snapshot(src, w, h)
    if data is None:
        return {"ok": False, "msg": err}

    # ★2026-10-23 低危修复: 同 test_tts —— 唯一临时名 + 清 60s 前遗留
    for _pat in ("/tmp/_wb_ocr_full_*.jpg", "/tmp/_wb_ocr_roi_*.jpg"):
        for _old in glob.glob(_pat):
            try:
                if time.time() - os.path.getmtime(_old) > 60:
                    os.remove(_old)
            except OSError:
                pass
    fd, full = tempfile.mkstemp(prefix="_wb_ocr_full_", suffix=".jpg")
    os.close(fd)
    fd, roi_f = tempfile.mkstemp(prefix="_wb_ocr_roi_", suffix=".jpg")
    os.close(fd)
    try:
        with open(full, "wb") as f:
            f.write(data)
    except OSError as e:
        return {"ok": False, "msg": "写临时帧失败: %s" % e}

    x, y, rw, rh = [int(v) for v in cfg["roi"]]
    if rw <= 0 or rh <= 0:
        return {"ok": False, "msg": "ROI 未设置（先在预览图上拖框）"}
    # ROI 是「配置采集分辨率」尺度(如 1920x1080); 经服务握手拿到的预览帧可能是
    # 降采样过的(如 960x540, 主程序 PREVIEW_DIV=2) —— 必须按实际帧尺寸换算并
    # 收敛, 否则 ffmpeg crop 直接报 "Invalid too big or non positive size"
    # (2026-10-07 实测)。ffprobe 与 ffmpeg 同装, 无新增依赖。
    p = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0",
                        "-show_entries", "stream=width,height",
                        "-of", "csv=s=x:p=0", full],
                       capture_output=True, timeout=10)
    try:
        fw, fh = (int(v) for v in p.stdout.decode().strip().split("x"))
    except ValueError:
        return {"ok": False, "msg": "识别帧尺寸失败(ffprobe): %s"
                % p.stderr.decode("utf-8", "replace").strip()[:120]}
    cw = int(cfg.get("capture_width") or fw) or fw
    ch = int(cfg.get("capture_height") or fh) or fh
    sx, sy = fw / cw, fh / ch
    x = max(0, min(int(x * sx), fw - 8))
    y = max(0, min(int(y * sy), fh - 8))
    rw = max(8, min(int(rw * sx), fw - x))
    rh = max(8, min(int(rh * sy), fh - y))
    p = subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", full,
                        "-vf", "crop=%d:%d:%d:%d" % (rw, rh, x, y), roi_f],
                       capture_output=True, timeout=30)
    if p.returncode != 0:
        return {"ok": False, "msg": "裁剪失败: %s"
                % p.stderr.decode("utf-8", "replace").strip()[:200]}

    lang = cfg["ocr"].get("lang", "chi_sim")
    psm = int(cfg["ocr"].get("psm", 11))
    t0 = time.time()
    try:
        q = subprocess.run(["tesseract", roi_f, "stdout", "-l", lang, "--psm", str(psm)],
                           capture_output=True, timeout=300)
    except subprocess.TimeoutExpired:
        return {"ok": False, "msg": "OCR 超过 300 秒（ROI 太大，请缩小到只框号码行）"}
    dt = time.time() - t0
    raw = q.stdout.decode("utf-8", "replace")

    cands = []
    for pat in cfg["ocr"].get("pattern", []):
        try:
            for m in re.finditer(pat, raw):
                g = m.groups()
                cands.append({"number": g[0] if g else "",
                              "window": g[1] if len(g) > 1 else ""})
        except re.error:
            continue
    return {"ok": True, "elapsed": round(dt, 2), "raw": raw.strip()[:400],
            "candidates": cands, "roi": [x, y, rw, rh], "pixels": rw * rh}


# ---------------------------------------------------------------- handler

# POST 请求体上限: 表单 JSON 1~2KB, 64KB 宽裕百倍(见 Handler._read_body)
MAX_BODY_BYTES = 64 << 10


def _same_origin(origin, referer, host):
    """CSRF 纵深防御(2026-10-22 第四轮): 判断浏览器请求是否同源。

    恶意网页诱导访客浏览器向本服务发 POST 时, 浏览器会带上指向攻击者站点的
    Origin(现代浏览器, 跨站 POST 必带)或 Referer(老浏览器) —— 与 Host 头比对
    即可识破, 攻击者无法伪造受害者浏览器发出的这两个头。
    无 Origin 也无 Referer = 非浏览器客户端(curl/现场排查脚本), 不在 CSRF
    威胁模型内, 放行 —— 兼容 tools/ 里直接 POST 的脚本。
    Origin 为 "null" 是沙箱 iframe/跨源跳转的特征, 明确拒绝。
    """
    cand = origin or ""
    if not cand:
        cand = referer or ""
    if not cand:
        return True  # 无 Origin 无 Referer = 非浏览器客户端, 放行
    if not host:
        # 有浏览器特征(Origin/Referer)却连 Host 都没有: 真实浏览器必带 Host,
        # 这种组合不是正常访问, 防御优先直接拒。
        return False
    if cand.strip().lower() == "null":
        return False
    try:
        netloc = urllib.parse.urlparse(cand).netloc.lower()
    except ValueError:
        return False
    return bool(netloc) and netloc == host.strip().lower()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):  # 静默默认访问日志, journal 只记关键事件
        pass

    def _json(self, obj, code=200):
        data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        try:
            if u.path == "/":
                body = PAGE.encode("utf-8")
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif u.path == "/api/config":
                # rev 随配置下发(乐观锁): 保存时必须原样带回, 被 409 挡住就刷新重载。
                # fillForm 按字段名取值, 多出的 rev 键无副作用。
                self._json(dict(load_cfg(), rev=config_rev()))
            elif u.path == "/api/status":
                st, tail = status_and_log()
                self._json(dict(st, log=tail))
            elif u.path == "/api/devices":
                self._json({"devices": list_usb_cams()})
            elif u.path == "/api/audio":
                self._json(audio_status())
            elif u.path == "/api/time":
                self._json(time_status())
            elif u.path == "/api/network":
                self._json(net_status())
            elif u.path == "/api/logs":
                self._json(logs_status())
            elif u.path == "/api/snapshot":
                q = urllib.parse.parse_qs(u.query)
                cfg = load_cfg()
                src = (q.get("src") or [cfg["camera_source"]])[0]
                src = urllib.parse.unquote(src)
                src = norm_src(src)
                # ★安全: src 直接进 ffmpeg 命令, 必须过白名单(见 validate_rtsp 注释)。
                #   否则可构造伪协议读本机任意文件。
                if not src.isdigit() and src != "simulate":
                    ok, src_or_msg = validate_rtsp(src)
                    if not ok:
                        self._json({"error": "非法源: " + src_or_msg}, 400)
                        return
                    src = src_or_msg
                w = int((q.get("w") or [cfg["capture_width"]])[0])
                h = int((q.get("h") or [cfg["capture_height"]])[0])
                if not (320 <= w <= 3840 and 240 <= h <= 2160):
                    self._json({"error": "分辨率超出范围"}, 400)
                    return
                if src.isdigit():
                    # USB: 主程序独占设备, 直接开 ffmpeg 会 Device busy
                    # -> 主程序在跑(不论 systemd 还是无剧本手工拉起)就请它落盘
                    #    最新帧; 确实没跑才自己抓
                    if announce_running():
                        data, err = usb_preview_via_service()
                    else:
                        data, err = ffmpeg_snapshot(src, w, h)
                else:
                    data, err = ffmpeg_snapshot(src, w, h)
                if data is None:
                    self._json({"error": err}, 500)
                    return
                # ★必须显式禁缓存: 预览 URL 每次都是同一串(/api/snapshot?src=0&w=..&h=..),
                #   不加头会让浏览器直接复用缓存图, 表现为"抓帧预览抓的不是实时的"(实测踩过)。
                self.send_response(200)
                self.send_header("Content-Type", "image/jpeg")
                self.send_header("Content-Length", str(len(data)))
                self.send_header("Cache-Control", "no-store, no-cache, must-revalidate, max-age=0")
                self.send_header("Pragma", "no-cache")
                self.send_header("Expires", "0")
                self.end_headers()
                self.wfile.write(data)
            else:
                self._json({"error": "not found"}, 404)
        except Exception as e:  # noqa: BLE001 — 任何异常都回 JSON, 不挂连接
            try:
                self._json({"error": str(e)}, 500)
            except Exception:
                pass

    def _read_body(self):
        """读 POST 请求体, 返回 (bytes|None, 错误信息|None, http状态码)。

        ★2026-10-22 第五轮审查: 旧版 5 个 POST 端点对 Content-Length 不设限 ——
        负值(int("-1") 合法)让 read(-1) 阻塞到 EOF, 线程永久挂起; 超大 body 整段
        读进内存撑爆 announce-web 的 MemoryMax 被 cgroup 反复杀。一句表单 JSON
        1~2KB, 64KB 已宽裕百倍。
        """
        try:
            n = int(self.headers.get("Content-Length", 0))
        except ValueError:
            return None, "Content-Length 非法", 400
        if n < 0:
            return None, "Content-Length 非法", 400
        if n > MAX_BODY_BYTES:
            # body 没读就回 413: keep-alive 下连接不能复用, 必须断开重连
            self.close_connection = True
            return None, "请求体过大", 413
        try:
            return self.rfile.read(n), None, 0
        except OSError as e:
            return None, "读取请求体失败: %s" % e, 400

    def do_POST(self):
        u = urllib.parse.urlparse(self.path)
        # ★CSRF 防御(2026-10-22): 本服务无认证, 所有 POST 都是"改配置/断网/重启"
        #   级别的副作用操作, 必须先过同源校验(独立成 _same_origin 是为了能进单测)。
        if not _same_origin(self.headers.get("Origin"),
                            self.headers.get("Referer"),
                            self.headers.get("Host") or ""):
            self._json({"error": "拒绝跨站请求: Origin 与本服务不符(若从书签/外站跳转而来请直接访问本页)"}, 403)
            return
        if u.path == "/api/network/confirm":
            try:
                p = subprocess.run(["sudo", "-n", NET_SCRIPT, "confirm"],
                                   capture_output=True, timeout=20)
                ok = p.returncode == 0
                self._json({"ok": ok, "msg": "已确认保留当前网络配置"
                            if ok else "confirm 失败: " +
                            (p.stderr.decode("utf-8", "replace").strip() or "code %s" % p.returncode)})
            except Exception as e:  # noqa: BLE001
                self._json({"ok": False, "msg": str(e)}, 500)
            return
        if u.path == "/api/network":
            raw, berr, bcode = self._read_body()
            if berr:
                self._json({"ok": False, "msg": berr}, bcode)
                return
            try:
                body = json.loads(raw.decode("utf-8"))
            except Exception as e:  # noqa: BLE001
                self._json({"ok": False, "msg": "请求体非法: %s" % e}, 400)
                return
            ok, m = net_apply(body)
            self._json({"ok": ok, "msg": m} if ok else {"error": m},
                       200 if ok else 400)
            return
        if u.path == "/api/logs":
            raw, berr, bcode = self._read_body()
            if berr:
                self._json({"ok": False, "msg": berr, "logs": logs_status()}, bcode)
                return
            try:
                body = json.loads(raw.decode("utf-8")) if raw else {}
            except Exception:  # noqa: BLE001
                body = {}
            ok, m = logs_action(str(body.get("action") or ""))
            self._json({"ok": ok, "msg": m, "logs": logs_status()}, 200 if ok else 400)
            return
        if u.path == "/api/test_ocr":
            # ★2026-10-23 低危修复: test_ocr 内部(ffprobe/tesseract 缺失、pattern
            #   非字符串、config.json 损坏)异常会直接逃逸 —— 客户端无响应、连接硬断。
            try:
                self._json(test_ocr(load_cfg()))
            except Exception as e:  # noqa: BLE001
                self._json({"ok": False, "msg": "测试 OCR 失败: %s" % e}, 500)
            return
        if u.path == "/api/test_tts":
            raw, berr, bcode = self._read_body()
            if berr:
                self._json({"ok": False, "msg": berr}, bcode)
                return
            try:
                body = json.loads(raw.decode("utf-8")) if raw else {}
            except Exception:  # noqa: BLE001
                body = {}
            cfg = load_cfg()
            # 「测试播报」允许带上页面当前填写的试听参数（未保存也能先听）:
            # 只覆盖无副作用字段、不写盘, 试听完页面保存与否都不受影响 —— 详见
            # apply_tts_preview_overrides（独立成函数是为了能进单测）。
            # ★2026-10-23 低危修复: body 传数组/数字、config 的 gap_sec 非法等
            #   也会抛异常逃逸 —— 与 test_ocr 同样兜底。
            try:
                apply_tts_preview_overrides(cfg, body.get("tts") or {})
                ok, m, text = test_tts(cfg, body.get("number") or "GA200",
                                       body.get("window") or "08")
            except Exception as e:  # noqa: BLE001
                self._json({"ok": False, "msg": "测试播报失败: %s" % e}, 500)
                return
            self._json({"ok": ok, "msg": m, "text": text, "audio": audio_status()})
            return
        if u.path == "/api/audio_fix":
            # ★2026-10-23 低危修复: 同上兜底(amixer/aplay 缺失等)
            try:
                ok, m = audio_fix()
            except Exception as e:  # noqa: BLE001
                self._json({"ok": False, "msg": "音频修复失败: %s" % e}, 500)
                return
            self._json({"ok": ok, "msg": m, "audio": audio_status()})
            return
        if u.path == "/api/time":
            raw, berr, bcode = self._read_body()
            if berr:
                self._json({"ok": False, "msg": berr}, bcode)
                return
            try:
                body = json.loads(raw.decode("utf-8")) if raw else {}
            except Exception:  # noqa: BLE001
                body = {}
            ok, m, rid = time_submit(body if isinstance(body, dict) else {})
            self._json({"ok": ok, "msg": m, "id": rid, "time": time_status()},
                       200 if ok else 400)
            return
        if u.path != "/api/save":
            self._json({"error": "not found"}, 404)
            return
        try:
            raw, berr, bcode = self._read_body()
            if berr:
                self._json({"error": berr}, bcode)
                return
            body = json.loads(raw.decode("utf-8"))
        except Exception as e:  # noqa: BLE001
            self._json({"error": "请求体非法: %s" % e}, 400)
            return
        # ★乐观锁(2026-10-22): 两个浏览器窗口同时编辑时, 后保存者会用自己页面
        #   加载时的旧快照整份覆盖, 先保存者的改动被静默丢掉。页面带上加载时的
        #   内容指纹 __rev, 与当前不一致就拒绝 —— 提示刷新重载, 不做自动合并
        #   (配置字段间有关联, 盲合并同样危险)。不带 __rev 的调用(脚本工具)不拦,
        #   保持兼容。
        got_rev = None
        if isinstance(body, dict):
            got_rev = body.pop("__rev", None)
            if got_rev and got_rev != config_rev():
                self._json({"error": "配置已被其他窗口修改，为避免覆盖他人改动已拒绝保存；"
                            "请刷新页面重新加载后再编辑"}, 409)
                return
        cfg, err = validate_and_build(body)
        if cfg is None:
            self._json({"error": err}, 400)
            return
        save_cfg(cfg)
        # 优先走工作时间调度脚本：它会「写 systemd timer + 按当前时刻校正运行状态」，
        # 未启用调度时退化为 restart，与旧行为等价。失败再退回直接重启，保证配置至少生效。
        sched = cfg.get("schedule") or {}
        try:
            p = subprocess.run(SCHED_CMD, capture_output=True, timeout=30)
        except (subprocess.TimeoutExpired, OSError):
            p = None
        if p is None or p.returncode != 0:
            try:
                pr = subprocess.run(RESTART_CMD, capture_output=True, timeout=20)
            except subprocess.TimeoutExpired:
                self._json({"error": "重启命令超时（服务可能仍在拉起，稍后刷新查看）"}, 500)
                return
            if pr.returncode != 0:
                e = pr.stderr.decode("utf-8", "replace").strip() or ("code %s" % pr.returncode)
                self._json({"error": "配置已写入，但重启失败: " + e +
                            "（新配置将在下次重启生效）"}, 500)
                return
            note = ""
            if p is not None:
                note = "（工作时间调度未生效: %s）" % (
                    p.stderr.decode("utf-8", "replace").strip() or ("code %s" % p.returncode))
            self._json({"ok": True, "msg": "配置已保存并重启%s。camera_source=%r roi=%s"
                        % (note, cfg["camera_source"], cfg["roi"]),
                        "rev": config_rev()})
            return
        if sched.get("enabled"):
            sinfo = "工作时间 %s ~ %s（非工作时间自动停机）" % (
                sched.get("work_start"), sched.get("work_end"))
        else:
            sinfo = "未启用工作时间调度（全天运行）"
        self._json({"ok": True, "msg": "已保存并生效。%s；camera_source=%r roi=%s"
                    % (sinfo, cfg["camera_source"], cfg["roi"]),
                    "rev": config_rev()})


def main():
    srv = ThreadingHTTPServer(("0.0.0.0", PORT), Handler)
    print("[config_web] listening on 0.0.0.0:%d, config=%s" % (PORT, CONFIG), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
