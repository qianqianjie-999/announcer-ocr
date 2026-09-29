# -*- coding: utf-8 -*-
"""announce.py — 叫号屏摄像头 OCR 语音播报（主程序）
链路: 摄像头(锁定曝光/对焦) -> ROI 帧差变化检测 -> 触发OCR -> 正则校验
      -> 多轮观察确认(非阻塞状态机) -> 去重 -> FIFO队列 -> 本地TTS播报2次 -> 日志留痕
设计要点: 确认过程不阻塞主循环, 屏幕连续跳号不会进"盲区";
         播报在线程内排队顺序执行, 播报中来的新号自动排队不会漏、不会乱序。
用法: python calibrate.py 先框选ROI一次, 然后 python announce.py
依赖: pip install opencv-python pyttsx3 rapidocr-onnxruntime
"""
import csv
import datetime as dt
import json
import os
import queue
import re
import tempfile
import threading
import time

import cv2

CFG = "config.json"

# ---------------------------------------------------------------- OCR 引擎自动选择
def make_ocr():
    try:
        from rapidocr_onnxruntime import RapidOCR
        engine = RapidOCR()
        return lambda img: rapid_ocr(engine, img)
    except ImportError:
        pass
    try:
        from paddleocr import PaddleOCR
        engine = PaddleOCR(use_angle_cls=False, lang="ch", show_log=False)
        return lambda img: paddle_ocr(engine, img)
    except ImportError:
        raise SystemExit("未安装OCR引擎: pip install rapidocr-onnxruntime (推荐,轻量) 或 paddleocr")

def rapid_ocr(engine, img):
    result, _ = engine(img)
    return " ".join(r[1] for r in result) if result else ""

def paddle_ocr(engine, img):
    result = engine.ocr(img, cls=False)
    lines = result[0] or []
    return " ".join(l[1][0] for l in lines)

# ---------------------------------------------------------------- TTS
def make_tts_fn(cfg):
    """按 config.tts.engine 选择发声函数，auto 时按 piper > espeak > pyttsx3 探测。
    返回 speak(text, times)：一次调用完整播放 times 遍。"""
    import shutil
    import subprocess
    t = cfg["tts"]
    eng = t.get("engine", "auto")

    def play(wav, times):
        """播放 wav：优先用配置的 ALSA 设备，被占用时自动退回 default。
        Linux 上有 pipewire/pulse 时会独占声卡，直连 plughw 会 Device busy。"""
        devs = []
        if t.get("aplay_device"):
            devs.append(t["aplay_device"])
        devs += [None, "default"]          # 依次回退
        last_err = None
        for dev in devs:
            cmd = ["aplay", "-q"] + (["-D", dev] if dev else []) + [wav]
            try:
                for _ in range(times):
                    subprocess.run(cmd, check=True, capture_output=True,
                                   timeout=120)
                    time.sleep(float(t.get("gap_sec", 0.35)))
                return True
            except Exception as e:
                last_err = e
        raise RuntimeError(f"aplay 播放失败(已尝试 {devs}): {last_err}")

    def piper_fn():
        binp = t.get("piper_bin") or shutil.which("piper")
        model = t.get("piper_model")
        if not (binp and model and os.path.exists(model)):
            return None
        # piper 需要同目录下的动态库与 espeak-ng-data，必须显式给出
        libdir = t.get("piper_lib_dir") or os.path.dirname(os.path.abspath(binp))
        env = dict(os.environ)
        env["LD_LIBRARY_PATH"] = libdir + os.pathsep + env.get("LD_LIBRARY_PATH", "")
        env["ESPEAK_DATA_PATH"] = libdir
        def fn(text, times):
            wav = os.path.join(tempfile.gettempdir(), "tts_piper.wav")
            subprocess.run([binp, "--model", model, "--output_file", wav],
                           input=text.encode("utf-8"), check=True,
                           capture_output=True, env=env, timeout=180)
            play(wav, times)
        fn.__name__ = "piper"
        return fn

    def espeak_fn():
        exe = shutil.which("espeak-ng") or shutil.which("espeak")
        if not exe:
            return None
        speed = max(80, int(t.get("rate", 180) * 0.7))  # espeak用wpm, 取慢速
        def fn(text, times):
            wav = os.path.join(tempfile.gettempdir(), "tts_espeak.wav")
            # 先生成 wav 再 aplay，避免 espeak 自己抓声卡与声音服务器冲突
            subprocess.run([exe, "-v", "zh", "-s", str(speed), "-w", wav, text],
                           check=True, capture_output=True, timeout=120)
            play(wav, times)
        fn.__name__ = "espeak"
        return fn

    def win_fn():
        """Windows: 优先用 SAPI COM 同步播报(线程安全、播完才返回),
        失败再退回 pyttsx3。"""
        voice = None
        try:
            import pythoncom
            import win32com.client
            pythoncom.CoInitialize()          # worker线程必须自行初始化COM
            voice = win32com.client.Dispatch("SAPI.SpVoice")
            voice.Rate = max(-10, min(10, int((t.get("rate", 180) - 180) / 25)))
        except Exception:
            voice = None
        if voice is not None:
            def fn(text, times):
                for _ in range(times):
                    voice.Speak(text)         # 同步: 播完才返回
                    time.sleep(0.3)
            fn.__name__ = "sapi"
            return fn
        try:
            import pyttsx3
            engine = pyttsx3.init()
            engine.setProperty("rate", t.get("rate", 180))
        except Exception:
            return None
        def fn(text, times):
            for _ in range(times):
                engine.say(text)
                engine.runAndWait()
            time.sleep(0.6)
        fn.__name__ = "pyttsx3"
        return fn

    order = {"piper": [piper_fn], "espeak": [espeak_fn], "win": [win_fn]}.get(
        eng, [piper_fn, espeak_fn, win_fn])
    for make in order:
        fn = make()
        if fn:
            print(f"[TTS] 使用引擎: {fn.__name__}")
            return fn
    raise SystemExit("未找到可用TTS: Linux 请安装 espeak-ng 或 piper-tts")


# ---------------------------------------------------------------- 播报文本
_DIGIT_CN = {"0": "零", "1": "一", "2": "二", "3": "三", "4": "四",
             "5": "五", "6": "六", "7": "七", "8": "八", "9": "九"}


def spell_tokens(s):
    """号码/窗口号逐字符展开: GA204 -> G A 二 零 四, C06 -> C 零 六。
    逐字念是人耳最不易听错的读法(窗口号 06 与 6 不会混淆)。"""
    return " ".join(_DIGIT_CN.get(c, c) for c in s)


class Announcer:
    """专用音频线程播报：多次播报顺序排队，单次失败自动蜂鸣。"""

    def __init__(self, cfg):
        self.cfg = cfg
        t = cfg["tts"]
        self.enabled = t.get("enabled", True)
        self.repeat = int(t.get("repeat", 2))
        self.template = t.get("template", "请{number}到{window}号窗口办理")
        self.spell = t.get("spell_number", False)
        self.queue = queue.Queue()
        self.worker = threading.Thread(target=self._loop, daemon=True)
        self.worker.start()

    def _loop(self):
        speak_fn = None
        if self.enabled:
            try:
                speak_fn = make_tts_fn(self.cfg)
            except Exception as e:
                print(f"[警告] TTS初始化失败({e})，仅蜂鸣提示")
        while True:
            number, window = self.queue.get()
            if self.spell:
                number, window = spell_tokens(number), spell_tokens(window)
            text = self.template.format(number=number, window=window)
            print(f"[播报] {text}")
            try:
                if speak_fn:
                    speak_fn(text, self.repeat)
                elif self.enabled:
                    # 仅在"开了语音但发声函数没建起来"时蜂鸣兜底。
                    # enabled=false 是用户主动要静音, 此时不该再响。
                    self._beep()
            except Exception as e:
                print(f"[错误] 播报失败: {e}")
                if self.enabled:
                    self._beep()
            self.queue.task_done()

    def speak(self, number, window):
        """入FIFO队列: 单播报线程顺序消费, 播报中来的新号自动排队, 顺序不乱。
        队列积压说明叫号过密, 只告警不丢号(声音会顺延)。"""
        depth = self.queue.qsize()
        if depth >= 3:
            print(f"[警告] 播报队列积压 {depth} 条, 叫号过于密集, 声音将顺延")
        self.queue.put((number, window))

    def _beep(self):
        try:
            import winsound
            for _ in range(3):
                winsound.Beep(1000, 300)
        except Exception:
            pass

# ---------------------------------------------------------------- 单实例锁
INSTANCE_PORT = 51888


def acquire_single_instance_lock(port=INSTANCE_PORT):
    """绑定本机回环端口作为单实例锁。端口被占 -> 已有实例在跑。
    为什么必须防重复启动: USB 摄像头是独占资源, 两个实例同时读取会让画面错乱,
    实测 OCR 输出直接崩坏(GA 被读成"青"/"精"/"M", 窗被读成"富"), 号码全部解析不出。
    为什么用端口而不是锁文件: 进程被杀/崩溃时操作系统自动释放端口,
    不会留下"死锁文件"导致下次正常启动被拒。"""
    import socket
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        # 故意不加 SO_REUSEADDR: Windows 下它反而允许绑定到已被占用的端口
        s.bind(("127.0.0.1", port))
        s.listen(1)
    except OSError:
        s.close()
        return None
    return s


# ---------------------------------------------------------------- 日志
def log(cfg, number, window, raw, status):
    """写一条播报日志。
    可靠性约定: 日志写失败绝不允许带崩播报主程序 —— 文件被杀毒/预览等
    短暂占用时(实测 PermissionError 直接把常驻进程崩掉), 先重试, 仍失败
    则打印告警并丢弃该条日志。"""
    row = [dt.datetime.now().isoformat(timespec="seconds"),
           number, window, raw, status]
    for attempt in range(3):
        try:
            with open(cfg["log_file"], "a", newline="", encoding="utf-8-sig") as f:
                csv.writer(f).writerow(row)
            return
        except OSError as e:
            if attempt < 2:
                time.sleep(0.3 * (attempt + 1))   # 0.3s / 0.6s 后重试
            else:
                print(f"[警告] 日志写入失败(已重试3次, 本条丢弃): {e}", flush=True)


# ---------------------------------------------------------------- 低延迟取流
class LatestFrame:
    """RTSP 低延迟取流: 后台线程以满帧率持续 grab, 主循环永远取"最新一帧"。
    必要性: 主循环只在 fps=3 时轮询, 若不后台排空, RTSP 解码缓冲会不断积压,
    延迟随时间从 0.3 秒涨到数秒(现场最隐蔽的坑)。"""

    def __init__(self, cap, grab_interval=0.03):
        self.cap = cap
        self.grab_interval = grab_interval
        self.lock = threading.Lock()
        self.frame = None
        self.ok = False
        self.seq = 0                 # 递增序号: 主循环可判断帧是否更新
        self.running = True
        self.thread = threading.Thread(target=self._loop, daemon=True)
        self.thread.start()

    def _loop(self):
        while self.running:
            ok, f = self.cap.read()
            with self.lock:
                if ok:
                    self.frame = f
                    self.seq += 1
                self.ok = ok
            time.sleep(self.grab_interval)

    def read(self):
        with self.lock:
            if not self.ok or self.frame is None:
                return False, None
            return True, self.frame.copy()

    def release(self):
        self.running = False


# ---------------------------------------------------------------- 主循环
def main():
    cfg = json.load(open(CFG, encoding="utf-8"))
    # 锁端口可配置: 便于在【不打扰正在运行的生产实例】的前提下另起一个
    # 模拟/回放实例做自测(config.camera_source="simulate" + 另一个 lock_port)。
    lock_port = int(cfg.get("lock_port", INSTANCE_PORT))
    hold_lock = acquire_single_instance_lock(lock_port)   # 必须保持引用, 否则端口会被回收
    if hold_lock is None:
        raise SystemExit(
            f"[退出] 检测到已有另一个 announce.py 实例在运行(占用 127.0.0.1:{lock_port})。\n"
            "       摄像头是独占资源, 重复启动会让画面错乱、OCR 全部识别失败。\n"
            "       如需重启, 请先结束旧进程再启动本程序。")

    if not cfg.get("roi"):
        raise SystemExit("请先运行 calibrate.py 框选识别区域")

    ocr = make_ocr()
    announcer = Announcer(cfg)
    # 支持配置多条正则(按序尝试后取并集): 现场 OCR 误读五花八门,
    # 一条写死容易整条漏播。先严格后宽松, 逐步兜底。
    _pats = cfg["ocr"]["pattern"]
    if isinstance(_pats, str):
        _pats = [_pats]
    pats = [re.compile(p) for p in _pats]

    # camera_source 支持: 0/1 等USB索引，或 "rtsp://user:pass@ip:554/..." 监控相机
    src = cfg.get("camera_source", cfg.get("camera_index", 0))
    if isinstance(src, str) and src.isdigit():
        src = int(src)
    sim_mode = (src == "simulate")   # 模拟模式: 读 sim_frame.jpg 当虚拟摄像头
    cap = None
    grabber = None
    is_net = isinstance(src, str) and src.lower().startswith(("rtsp://", "http://", "https://"))
    if not sim_mode:
        if is_net:
            # RTSP 低延迟: 关缓冲/低延迟解码/丢弃重排队列, 再开后台排空线程
            os.environ["OPENCV_FFMPEG_CAPTURE_OPTIONS"] = (
                "rtsp_transport;tcp|fflags;nobuffer|flags;low_delay"
                "|max_delay;300000|reorder_queue_size;0"
            )
        cap = cv2.VideoCapture(src)
        if is_net:
            cap.set(cv2.CAP_PROP_BUFFERSIZE, 1)
            if cfg.get("low_latency", True):
                grabber = LatestFrame(cap)
                time.sleep(1.0)          # 等首帧到位
        else:  # USB相机: 锁定自动曝光/对焦防漂移; 监控相机请在相机Web管理页设置
            cap.set(cv2.CAP_PROP_AUTO_EXPOSURE, 0.25)
            cap.set(cv2.CAP_PROP_AUTOFOCUS, 0)
            focus = cfg.get("camera_focus")
            if focus is not None:   # 手动对焦扫描得到的最佳值, 防自动对焦失焦导致全画面糊
                cap.set(cv2.CAP_PROP_FOCUS, int(focus))
                # 回读校验: 实测本机USB相机请求 150 只写进 128(驱动钳制/量化),
                # "锁焦"悄然失效却毫无提示。回读不一致必须告警, 否则标定白做。
                try:
                    got = cap.get(cv2.CAP_PROP_FOCUS)
                    if abs(got - float(focus)) > 2:
                        print(f"[警告] 对焦请求 {int(focus)} 未生效, 硬件实际为 {got:.0f}。"
                              f"该相机对焦范围与标定值不符, 建议用 focus_scan.py 重扫,"
                              f"并把 config.camera_focus 改成硬件能接受的值。",
                              flush=True)
                except Exception:
                    pass
            cap.set(cv2.CAP_PROP_FRAME_WIDTH, cfg.get("capture_width", 1920))
            cap.set(cv2.CAP_PROP_FRAME_HEIGHT, cfg.get("capture_height", 1080))

    x, y, w, h = cfg["roi"]
    det = cfg["detect"]
    interval = 1.0 / cfg.get("fps", 3)
    prev_gray = None
    announced = set()       # 已播报去重
    # 非阻塞确认状态机: {候选: [累计轮数, 最后出现时刻, 首次出现时刻]}
    # 关键: 不在确认期间 sleep 阻塞主循环, 否则那几秒内屏幕再变化会被"盲区"吞掉
    confirm = {}
    need_stable = int(det.get("stable_frames", 2))
    ttl = float(det.get("confirm_ttl_sec", 4.0))     # 多久没再出现就遗忘该候选
    active_sec = float(det.get("active_sec", 3.0))   # 出现候选后保持"观察期"的时长
    fb_sec = float(det.get("fallback_ocr_sec", 3.0))  # 兜底 OCR 间隔(保险丝)
    last_ocr_time = 0.0
    active_until = 0.0
    # 诊断心跳: 每 diag_every 秒报一次"近N秒最大帧差", 用来判断画面变化
    # 究竟有没有被看见。排查"举了卡却没反应"时靠它区分【没举卡】和【阈值漏检】。
    diag_on = bool(cfg.get("diag_heartbeat", True))
    diag_every = float(cfg.get("diag_every_sec", 5.0))
    diag_last = time.time()
    diag_max = 0.0
    diag_frames = 0
    # 触发存证: 每次因"画面变化"跑 OCR 时, 把 OCR 实际看到的画面存成图片。
    # 排查「心跳有变化但读不出」时直接看图定位(糊/暗/太远), 不用再猜、也不用掐时间举卡。
    save_frames = bool(cfg.get("save_trigger_frames", True))
    save_dir = "_live"
    last_save = 0.0
    n_saved = 0

    def read_frame():
        if sim_mode:
            f = cv2.imread("sim_frame.jpg")
            return (f is not None), f
        if grabber is not None:
            return grabber.read()      # 永远取最新帧, 不积压
        return cap.read()

    def norm_digits(s):
        """数字段归一化: OCR常把0读成O、1读成I/l"""
        return s.replace("O", "0").replace("I", "1").replace("l", "1")

    def parse_now(txt):
        out = set()
        flat = txt.replace(" ", "")
        for p in pats:                      # 多条正则取并集, 任一命中即算
            for m in p.finditer(flat):
                num = m.group(1)
                m2 = re.match(r"([A-Z]{1,2})(.*)", num)  # 前缀字母保留, 其余归一化
                if m2:
                    num = m2.group(1) + norm_digits(m2.group(2))
                out.add((num, norm_digits(m.group(2))))
        return out

    print(f"运行中: 源={src}, ROI={cfg['roi']}, 稳定帧数={det['stable_frames']}, Ctrl+C 退出")
    while True:
        loop_start = time.time()
        ok, frame = read_frame()
        if not ok:
            print("[错误] 取帧失败，1秒后重试")
            time.sleep(1)
            prev_gray = None
            continue

        roi = frame[y:y + h, x:x + w]
        gray = cv2.cvtColor(roi, cv2.COLOR_BGR2GRAY)
        gray = cv2.resize(gray, (min(w, 480), max(1, int(h * min(w, 480) / w))))
        gray = cv2.GaussianBlur(gray, (5, 5), 0)

        changed = True
        ratio = None
        if prev_gray is not None and prev_gray.shape == gray.shape:
            # 先抵消"整体亮度漂移"再比: 摄像头自动曝光会持续微调, 让整帧同时
            # 变亮/变暗。原始 absdiff 会把这种漂移当成"画面变化", 于是空场景也
            # 反复触发 OCR(实测 40 秒 46 条 INVALID), 逼得阈值必须调高; 而阈值
            # 一高, 真实场景"屏上只换了几个号码"那种 1% 量级的微小变化就被漏掉。
            # 对齐均值后再比, 漂移归零, 阈值才敢压低。
            shift = float(gray.mean()) - float(prev_gray.mean())
            ref = (cv2.convertScaleAbs(prev_gray, alpha=1.0, beta=shift)
                   if abs(shift) >= 1.0 else prev_gray)
            diff = cv2.absdiff(gray, ref)
            ratio = float((diff > det["diff_threshold"]).mean())
            changed = ratio > det["changed_ratio"]
        prev_gray = gray

        now = time.time()

        # —— 诊断心跳 ————————————————————————————————————————
        # 判读: 举卡瞬间应看到 最大帧差 跳到 阈值 之上; 若始终贴近 0,
        #       说明"画面根本没变化"(没举卡/镜头没对着), 而非阈值漏检。
        if ratio is not None:
            diag_max = max(diag_max, ratio)
        diag_frames += 1
        if diag_on and now - diag_last >= diag_every:
            span = now - diag_last
            tag = ">>> 有变化" if diag_max > det["changed_ratio"] else "静"
            print("[监测] 近%.0fs 最大帧差=%.4f 阈值=%.4f 帧数=%d %s"
                  % (span, diag_max, det["changed_ratio"], diag_frames, tag), flush=True)
            diag_last = now
            diag_max = 0.0
            diag_frames = 0
        # 触发条件, 三条路径并联:
        #   ① 帧差判定有变化            -> 快速路径(省 CPU 的主力)
        #   ② 处于观察期(有候选待确认)  -> 完成多轮确认
        #   ③ 距上次 OCR 超过 fb_sec    -> 兜底复查(保险丝)
        # 为什么必须有③: 实测"屏上只换了几个号码"的帧差只到 0.0023 量级,
        # 若 ROI 框得比屏大, 帧差可能低到检测不到 -> 漏播。低频兜底保证不漏,
        # 代价仅每 fb_sec 一次 OCR。有了它, 就不必靠"观察期无限续期"把 OCR
        # 变相常开(那样帧率会从 3fps 掉到 0.8fps, 白烧 CPU)。
        gap = now - last_ocr_time
        due_change = (changed or now < active_until) and gap >= det["min_interval_sec"]
        due_fallback = fb_sec > 0 and gap >= fb_sec      # fb_sec<=0 表示关闭兜底
        if due_change or due_fallback:
            last_ocr_time = now
            crop = frame[y:y + h, x:x + w]
            if save_frames and changed and now - last_save >= 2.0 and n_saved < 20:
                last_save, n_saved = now, n_saved + 1
                try:
                    os.makedirs(save_dir, exist_ok=True)
                    okb, buf = cv2.imencode(".jpg", crop,
                                            [int(cv2.IMWRITE_JPEG_QUALITY), 92])
                    if okb:
                        p = os.path.join(save_dir, "trig_%02d.jpg" % n_saved)
                        with open(p, "wb") as f:
                            f.write(buf.tobytes())
                        print("[存证] OCR所见画面 -> " + p, flush=True)
                except Exception as e:
                    print("[存证失败] " + str(e), flush=True)
            raw = ocr(crop)
            cands = parse_now(raw)
            if not cands and w <= 1280:
                # 兜底第二路: 小图放大2倍+增强对比再识别一次。
                # 远处/小字首轮极易读错(GA读成青/精, 窗读成富), 放大后命中率明显提升。
                up = cv2.resize(crop, None, fx=2.0, fy=2.0,
                                interpolation=cv2.INTER_CUBIC)
                g = cv2.cvtColor(up, cv2.COLOR_BGR2GRAY)
                up = cv2.cvtColor(cv2.createCLAHE(2.0, (8, 8)).apply(g),
                                  cv2.COLOR_GRAY2BGR)
                raw2 = ocr(up)
                if parse_now(raw2):
                    cands, raw = parse_now(raw2), raw2
            # 只对"尚未播报"的候选续观察期。若对已播报的号码也续期, 屏幕一直
            # 显示号码时观察期会被无限续期 -> OCR 实际常开, 既费 CPU 又掩盖了
            # "帧差阈值到底够不够低"这个问题(实测帧率 3fps -> 0.8fps)。
            if cands - announced:
                active_until = now + active_sec
            # 清理超时未再出现的候选(闪变残留), 不立即清零 -> 短暂被替换的号码仍保留计数
            confirm = {c: v for c, v in confirm.items() if now - v[1] <= ttl}
            for cand in sorted(cands):
                v = confirm.get(cand)
                cnt = v[0] + 1 if v else 1
                first = v[2] if v else now          # 首次出现时刻 = 真正的"屏幕变化"起点
                confirm[cand] = [cnt, now, first]
                if cnt >= need_stable and cand not in announced:
                    announced.add(cand)
                    log(cfg, cand[0], cand[1], raw, "OK")
                    announcer.speak(*cand)          # 入FIFO队列, 播报线程按顺序播
                    print(f"[时序] 首见→开始播报 {(time.time() - first) * 1000:.0f} ms"
                          f" (确认{cnt}轮); 另加相机/网络/轮询约 0.3~0.8 s")
            if not cands and changed:
                log(cfg, "", "", raw, "INVALID")

        time.sleep(max(0.05, interval - (time.time() - loop_start)))

if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n已退出")
