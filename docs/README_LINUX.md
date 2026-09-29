# Linux 小主机部署指引

## 前提
- Debian/Ubuntu 系（树莓派 OS、Armbian、Ubuntu Server 均可），x86 迷你主机或树莓派 4B/5；
- 音频输出：主机 3.5mm/HD-Audio 直连有源音箱，或插 USB 声卡；
- 摄像头：USB 摄像头插主机，或监控相机走 RTSP（更推荐，主机可以藏在机房）。

## 语音引擎怎么选（config.json → tts.engine）
| engine | 效果 | 要求 |
|---|---|---|
| `piper` | 好（神经网络中文女声，离线） | `pip install piper-tts` + 下载中文模型；小主机内存 ≥2GB 建议 |
| `espeak` | 机器味重但能用 | install.sh 已装 espeak-ng，开箱即用 |
| `win` | Windows 本机调试用 | 仅 Windows |
| `auto`（默认） | 自动按 piper > espeak > win 探测 | 装了哪个用哪个 |

想升级 piper：
```
venv/bin/pip install piper-tts
mkdir -p models && cd models
wget https://huggingface.co/rhasspy/piper-voices/resolve/main/zh/zh_CN/huayan/medium/zh_CN-huayan-medium.onnx
wget https://huggingface.co/rhasspy/piper-voices/resolve/main/zh/zh_CN/huayan/medium/zh_CN-huayan-medium.onnx.json
```
然后 `config.json` 里 `"engine": "piper"`、`"piper_model": "models/zh_CN-huayan-medium.onnx"`（HF 访问不动可换镜像或我先帮您下载好拷过去）。

## 部署步骤
```
1. 把整个「叫号屏摄像头OCR」目录拷到小主机，如 /opt/announcer/
2. sudo bash install.sh                 # 装依赖 + venv + systemd 自启
3. 配置识别区域（三选一）:
   a) 有桌面: venv/bin/python calibrate.py 拖框；
   b) 无桌面: 先在 Windows 电脑上跑 calibrate.py 出 config.json，再拷过来；
   c) 手填: config.json 的 roi: [x, y, 宽, 高]（先跑 snapshot 拍一张看画面定坐标）
4. 改 camera_source: USB 填 "/dev/video0"；监控相机填 RTSP 地址
5. sudo systemctl start announce
6. journalctl -u announce -f           # 看运行日志，叫个号验证播报
```

## 无桌面时先拍一张快照定 ROI
```
venv/bin/python -c "import cv2; c=cv2.VideoCapture(0); ok,f=c.read(); cv2.imwrite('view.jpg', f)"
```
把 `view.jpg` 拷回 Windows 电脑打开看像素坐标（画图工具即可），按 `roi: [左, 上, 宽, 高]` 填进 config.json。

## 低配主机专章（老 CPU / 内存 ≤2GB，如 Celeron J1850 / J1900 / N2840 等）
**先跑体检，别猜**：
```
python3 check_host.py       # 输出指令集支持、onnxruntime 可用性、实测OCR耗时、开口延迟推算
```

### 老平台的两个硬伤
1. **无 AVX/AVX2**（2013 年前的 Atom / Bay Trail / Core 2 全无）：深度学习推理只能退到 SSE2 内核，同频下比 AVX2 机器慢 2~4 倍；
2. **单核性能低**：J1850（Bay Trail-D，4核2.0GHz，2013年，CPU 焊死不可换）单核约为 i5-9400F 的 1/4。

两者叠加：**i5-9400F 上实测 0.63~0.95 秒的 OCR，在老平台上预计 4~8 秒**，开口延迟会到 8~16 秒。叫号后十几秒才出声，体验不可接受。

### 已实测否掉的三个"弱机优化"（别再走弯路）
| 想法 | 实测结果（i5-9400F） |
|---|---|
| 关掉文字检测模型（`use_det=False`）只跑识别 | ❌ **反而变慢**：950ms → 987ms（识别模型拿到整图后按原尺寸切条，输入高度暴涨） |
| ROI 缩成只框一行的窄条带 | ❌ **慢 3 倍**：950ms → 3003ms（检测模型按**短边**放大到 736，宽条带像素量爆炸） |
| ROI 降采样到一半 | ❌ 几乎无收益：950ms → 768ms（模型内部统一缩放） |

**结论：没有便宜的提速手段，短板只能靠换 CPU 解决。**

### 那这台老机器怎么用
- 跑 `叫号屏网口嗅探/02_sniff.py`：纯字符串解析，老 CPU 毫无压力，10W 功耗正合适 7×24；
- 或做其它轻量常驻服务（日志采集、定时任务）；
- **OCR 主机建议换 Intel N100 迷你主机（¥600~1300）**，同场景 0.8~1.5 秒/次，开口延迟 3.5~4.5 秒。

### 如果一定要在这台老机器上凑合
按这个组合把延迟压到最低：
```
detect.stable_frames = 2      # 确认轮数别再加
detect.min_interval_sec = 0.3
fps = 5                       # 轮询密一点，帧差检测极便宜
tts.engine = "espeak"         # 别用 piper，老 CPU 跟不上
tts.repeat = 1                # 省占线时间
摄像头用子码流 720p/5fps       # 老平台软解 1080p 要吃掉 1.5 个核
```
预计开口延迟仍在 8 秒以上，只适合"叫号不密集、不追求即时感"的场合。

## 常见坑
| 现象 | 处理 |
|---|---|
| 无声音 | `aplay -l` 看设备；`sudo usermod -aG audio <用户>` 后重新登录；HDMI 接屏时声音可能走了 HDMI，用 `aplay -D plughw:1,0` 指定 |
| 程序启动即崩、报 `Illegal instruction` | CPU 缺少 onnxruntime 需要的指令集 —— 先跑 `check_host.py` 确认，这台机器不适合做 OCR 主机 |
| 2GB 内存跑到 swap | 关桌面环境和其它服务；`tts.engine=espeak`；`stable_frames=2` |
| /dev/video0 打不开 | `sudo usermod -aG video <用户>` |
| USB 摄像头画质差 | 限 USB 供电不足，换带独立供电的口或用 RTSP |
| 树莓派上 OCR 慢 | rapidocr 首帧 3~5 秒，之后每次 0.5~1 秒，叫号场景可接受；介意就上 x86 迷你主机 |

## 文件
- `install.sh` —— 一键部署
- `announce.service` —— systemd 单元（install.sh 自动替换路径安装）
- `announce.py` —— 主程序（TTS 自动适配 Linux/Windows）
- `check_host.py` —— **部署前体检**（指令集/内存/onnxruntime可用性/实测OCR耗时）
- `bench_latency.py` —— 各环节耗时基准（OCR / 帧差 / TTS）
- `延迟分析.md` —— 延迟实测数据与调参对照
