# 叫号屏摄像头 OCR 语音播报 · 部署指引

> **项目说明**：通过一路摄像头（USB 或 RTSP 网络相机）对准医院/政务大厅的叫号屏，
> 用 OCR 识别「请 XXX 到 N 号窗口」文字，实时转成中文语音播报。
> 适用于既有叫号系统未提供音频输出、或需要独立二次播报的场景。零改造接入。
>
> - **Python 版**（`announce.py`）：功能全、依赖轻、调试方便，适合先跑通验证。
> - **Rust 版**（`announce-rs/`）：静态单文件二进制，内存占用低，适合 2 核低频小主机长期驻守。
> - 两版配置格式一致（`config.json`），可无缝切换。
>
> **仓库内容**：源码 + 通用部署物（systemd 单元 / 安装脚本 / Web 配置台 / 调试工具 / 自测）。
> 现场专用的远程运维脚本、抓帧存证、运行日志不入库（见 `.gitignore`）。


## 方案定位
与原叫号系统**零接触**：不接网线、不碰对方设备、不依赖对方任何接口。摄像头只"看"屏幕，原系统出问题归对方维修，权责清晰。

## 硬件清单（约 300~500 元）
| 物品 | 要求 | 说明 |
|---|---|---|
| 摄像头（二选一） | **USB**: 1080P 支持手动曝光/对焦（罗技 C270/C920 级）<br>**监控**: POE 枪机 200~400 万像素，海康/大华 | **优先监控摄像头**：画质锐、网线可传 50 米、工业级 7×24 不漂移。需 PoE 交换机或 12V 供电 |
| CPL 偏振镜 | 与镜头口径匹配 | **消玻璃反光的关键**，摄影店/网购几十元 |
| 固定支架 | 可锁紧万向臂 | 装好后严禁移位，动了需重新校准 |
| Windows 主机 | 任意 4 代 i5 以上 | 可用闲置电脑；开机自启本程序 |

### 用监控摄像头时的设置（相机 Web 管理页一次性配置）
- `config.json` 的 `camera_source` 填 RTSP 地址，如 `"rtsp://admin:密码@192.168.1.64:554/h264/ch1/main/av_stream"`（海康格式；大华为 `/h264/ch1`，具体见型号手册）；
- 关闭：自动增益、背光补偿、宽动态、红外夜视自动切换（室内恒定光线，防画面突变误触发）；
- 固定码率与主码流分辨率（1080P 以上），帧率 10~15fps 够用；
- 注意 RTSP 有 0.5~2 秒延迟，端到端播报约 2~5 秒，叫号场景可接受。

## 安装步骤
```
1. pip install opencv-python pyttsx3 rapidocr-onnxruntime
2. 支架固定摄像头，正对屏幕，距离以屏占画面 1/2 以上为宜，
   镜头稍偏开轴线+装 CPL 消反光（照片里那种重影就是反光导致）
3. python tools/focus_scan.py   -> 【USB摄像头必做】扫出手动对焦最佳值，填进 config 的 camera_focus
4. python tools/calibrate.py    -> 拖框选中上排"当前办理"文字区域（避开滚动字幕行）
5. python tools/preview.py      -> 实时预览，确认画面清晰、出现绿框 HIT（调机位用）
6. python announce.py           -> 到叫号主机叫一个号，验证听到 2 次播报
7. 开机自启：Win+R -> shell:startup -> 放入 announce.bat
```

> ⚠️ **USB 摄像头的自动对焦经常是失灵的**（实测某型号自动对焦锐度仅 19.7，手动扫到最佳值后 66.1，画面清晰度差 3 倍以上）。
> 表现就是"整个画面都糊、OCR 读不出"——**不是分辨率问题，也不是代码问题**。
> 传感器像素不够只会让画面颗粒粗，但不会糊成一片；糊，八成是对焦。先跑 `focus_scan.py`。

## 部署在哪（三种摆法，按现场条件选）

语音播报的音箱必须在大厅，这一点决定主机摆哪：

- **A. 大厅/服务台放一台闲置电脑或迷你主机（推荐）**：电脑直连一对有源音箱（约 50 元），取 RTSP 流播报，最简单最可靠。
- **B. 主机放机房/办公室 + IP 网络音柱**：大厅不让放设备时用——电脑在机房跑程序，买一台网络音柱（200~400 元）挂大厅，播报音频走网络推送（需给音柱接一路 RTSP/TTS 推流或改用音柱自带的对接方式，实施时再说）。
- **C. 蹭窗口现有电脑**：如果窗口有常开的电脑且允许装 Python+接音箱，零新增硬件；缺点是依赖别人机器，重启/重装会影响播报，建议签个简单使用约定。

摄像头侧统一接法：枪机装屏正对面/上方（稍偏轴+CPL 消反光）→ 网线回 PoE 交换机 → 交换机与运行电脑同网段即可，机房在哪无所谓。

## 关键参数（config.json）
| 参数 | 默认 | 调整时机 |
|---|---|---|
| `detect.diff_threshold` | 25 | 误触发频繁→调高；漏检→调低 |
| `detect.changed_ratio` | 0.005 | 画面轻微波动导致误触发→调高 |
| `detect.stable_frames` | 2 | 需连续确认的轮数。延迟想更短→1（基本失去抗闪变，不建议）；反光严重导致误播→3 |
| `detect.confirm_ttl_sec` | 4.0 | 候选多久没再出现就遗忘。**实际生效值 = max(此值, 3×本轮OCR耗时)**（OCR 慢时自动放宽，否则候选凑不齐 2 轮、永不播报） |
| `detect.active_sec` | 3.0 | 出现候选后继续保持观察的时长 |
| `ocr.pattern` | 请…到…号窗口 | 报文格式变化时改正则 |
| `tts.repeat` | 2 | 播报次数（不影响开口延迟，只影响占线时长） |
| `fps` | 3（弱机档建议 5） | 轮询帧率，调高可省延迟；强机档可到 10~15 |
| `low_latency` | true | RTSP 后台排空取流，防延迟累积，**别关** |
| `camera_focus` | 150 | **USB 摄像头手动对焦值**，由 `focus_scan.py` 扫出；`null` 表示不干预 |
| `detect.settle_ticks` | 3 | **停稳闸**：帧差变化后连续 N 轮无变化才跑 OCR。换卡/动画期间 OCR 结果必然作废，早跑纯属浪费 CPU；设 0 为旧行为（变化立即 OCR） |

> 延迟实测数据与调优建议见 `docs/延迟分析.md`。

## 性能档位（现场小主机配置可能变高，按机器选档）

项目默认按最低配现场（J1850 类 2 核低频小主机）调优。若现场换更强的机器，按下表升档即可，**改 config.json 就够，无需改代码**。

| 参数 | 弱机档（默认，2 核低频） | 强机档（4 核+/高频 x86 或新款 ARM） |
|---|---|---|
| `fps` | 5（实测平衡点） | 10~15 |
| `detect.settle_ticks`（停稳闸） | 3（≈0.6s 停稳判定） | 2（OCR 快，可更快进确认） |
| `detect.min_interval_sec` | 0.5 | 0.2~0.3 |
| `detect.fallback_ocr_sec` | 3.0 | 2.0 |
| ROI 采样分辨率 | 720P/子码流（RTSP 场景） | 1080P 可行，ROI 更准 |
| OCR 引擎 | tesseract（内存占用 <100MB） | 可换 PaddleOCR / rapidocr-onnxruntime（更准更快，内存 +300MB 级） |
| 实测换卡→出声 | 1.7~3.6s（平均 2.9s） | 预期 <1.5s（帧差更密 + OCR 提速） |

强机档注意事项：

1. **先升 fps 再看 CPU**：`top` 观察 CPU 占用，若单核打满 90%+ 说明该机不是强机，退回 5。
2. **RTSP 场景**：子码流升到 720P@10fps 通常无压力；升主码流前先确认解码占用（软解 1080P@25fps 会吃掉一整核，得不偿失）。
3. **TTS 不用动**：piper 常驻合成与机器档位无关，出声延迟由合成速度决定，两档差异 <0.2s。
4. **Rust 版交叉编译 target**：强机若是 x86_64 小主机，把构建脚本 target 换成 `x86_64-unknown-linux-musl`；新款 ARM 同理换 `aarch64-unknown-linux-musl`。静态二进制换机即拷即跑。
5. **确认链不用改**：静帧快速确认 + 停稳闸在两档下行为一致，强机上只会更快。

## 现场调试工具（识别不出问题时按顺序用，均在根目录运行）

| 工具 | 用途 | 什么时候用 |
|---|---|---|
| `tools/preview.py` | **实时预览 + 在线 OCR**：画面叠加清晰度数值、识别框、命中号码 | 装好摄像头后调机位/距离/角度；平时想看程序"看到什么" |
| `tools/focus_scan.py` | **手动对焦扫描**：0~255 逐档试，输出最佳焦点并写回配置 | 画面整体发糊、OCR 读不出时（USB 头首选动作） |
| `tools/snapshot.py` | 抓一帧 + 跑 OCR，打印"到底看到了什么字" | 程序一直记 INVALID，要分辨是没拍到 / 字太小 / 格式不匹配 |
| `tools/check_resolution.py` | 查摄像头真实分辨率与是否软件插值放大 | 怀疑画面被驱动放大充数时（PSNR>32dB 才是放大） |
| `tools/autocalib.py` | **无桌面环境**自动定位 ROI（抓帧→OCR→找叫号行→算ROI） | Linux 服务器无 GUI，不能用鼠标框选时 |
| `tests/bench_latency.py` / `tests/bench_machine.py` | 实测 OCR/帧差/TTS 各环节耗时 | 换机器后想知道实际延迟 |
| `deploy/check_host.py` | 目标主机体检（CPU指令集/内存/算力/OCR耗时） | 部署前评估"这台机器跑不跑得动" |

## 日常维护
- **每周看一眼** `announce_log.csv`：`INVALID` 行占比高说明反光/对焦漂了，重新 `calibrate.py` 即可；
- 摄像头画面被遮挡、摄像头掉线会自动重试并蜂鸣提示；
- 播报失败（TTS 异常）自动蜂鸣 3 声，日志记 `OK` 前的 `INVALID` 可回放排查漏报。

## 边界与责任
- 只读屏幕画面，不写不改原系统任何数据；
- 若 OCR 连续判不出（如对方换屏、改格式），程序不会误播，只会记录 INVALID 并继续等待；
- 建议与对方口头约定：屏幕显示内容保持现有格式不变。

---

# Rust 版（announce-rs）—— 低内存部署方案

## 与 Python 版的取舍
| | Python 版（announce.py） | Rust 版（announce-rs） |
|---|---|---|
| 主进程内存 | 700~900 MB（onnxruntime 占大头） | **峰值 16.7 MB**（实测，降幅约 98%） |
| 二进制/依赖 | 需装 Python + pip 依赖（约 1GB 磁盘） | **单个 2.4MB 静态二进制**，靶机零工具链 |
| OCR 引擎 | RapidOCR（onnxruntime，进程内） | 外部 `tesseract` CLI（chi_sim 简体包） |
| 取流 | OpenCV VideoCapture | 外部 `ffmpeg` 子进程（USB v4l2 / RTSP 同源低延迟参数） |
| TTS | pyttsx3/espeak | piper 中文女声（**常驻合成服务**，见下文）或 `espeak-ng` → wav → `aplay`（含 ALSA 设备回退） |
| 触发/去重/日志/存证 | 相同逻辑 | 逐项对齐（确认状态机、FIFO 播报线程、CSV 日志含 BOM、触发存证） |
| 适用场景 | 需要快速改逻辑、有桌面环境 | **小主机/树莓派等弱机长期值守** |

两版可共存，同配置语义（`config_rs.json` 为 Rust 版生产参考）。

## 构建方法（Windows 本机交叉编译 → 上传直接运行）
Rust 版全依赖为纯 Rust crate（serde/regex/image/chrono，无 C 绑定），可静态交叉编译到
`x86_64-unknown-linux-musl`，产物免 glibc 依赖，任意 x86_64 Linux 直接跑：

```
工具链（一次性，便携解压、不装 rustup）:
  1. 从 TUNA 镜像 dist/<日期>/ 下载四件 tar.xz 并合并解压到 C:\Users\fangf\wb-rust\root-gnu\:
     rustc-gnu + cargo-gnu + rust-std-gnu + rust-mingw   (host = x86_64-pc-windows-gnu)
  2. CARGO_HOME 指向自带 rsproxy sparse 源的目录
  3. 项目 .cargo/config.toml 指定 musl 目标 linker 为工具链自带 rust-lld.exe

编译（约 1~2 分钟）:
  $env:PATH = "C:\Users\fangf\wb-rust\root-gnu\bin;$env:PATH"
  $env:CARGO_HOME = "C:\Users\fangf\.workbuddy\binaries\rust\cargo"
  cd C:\Users\fangf\wb-rust\announce-rs
  cargo build --release --target x86_64-unknown-linux-musl

产物: target\x86_64-unknown-linux-musl\release\announce-rs   (~2.4MB, 静态)
```

> ⚠️ 踩坑记录：rustup 在沙箱/受限环境会因 hardlink 语义报 os error 183，便携解压可绕开；
> mingw ld 不支持非 ASCII 路径，工具链与源码必须放纯英文目录；
> musl 目标默认找系统 `cc`，需显式指定 rust-lld。

## 部署到 Debian 小主机（192.168.8.100）
```
1. 上传二进制与测试:
   python deploy\rs_deploy_test.py
   （sftp 推二进制到 /home/debian/anntest/ + 六阶段回放自检，见下）

2. 靶机外部依赖（均为系统包，与程序静态部分无关）:
   sudo apt install tesseract-ocr tesseract-ocr-chi-sim espeak-ng alsa-utils
   接真实摄像头另需: sudo apt install ffmpeg

3. 生产配置: deploy/config_rs.json（tts/psm/去重参数与 Python 版对齐后的定稿值）

4. systemd 常驻（三件套，均开机自启）:
   announce-piper.service  常驻语音合成（tts_server.py，仅 127.0.0.1:5001，MemoryMax=400M）
   announce-rs.service     播报主程序（MemoryMax=300M）
   announce-web.service    Web 配置台（8080）
   sudo cp deploy/announce-{rs,web,piper}.service /etc/systemd/system/
   sudo cp deploy/tts_server.py /home/debian/anntest/
   sudo systemctl daemon-reload && sudo systemctl enable --now announce-piper announce-rs announce-web
   注意: 与 Python 版服务二选一，不要同时占同一摄像头/声卡。
```

## 六阶段回放自检（更新版本后必跑）
`python deploy\rs_deploy_test.py` 一条命令完成：上传二进制 → 推 6 张阶段帧 + `tests/selftest_rs.py` → 靶机沙盒回放。

| 阶段 | 期望 |
|---|---|
| 1 空场启动 | 静默 |
| 2 举卡A（GA200→C08, GA999→C01） | 两条新增播报 |
| 3 重复卡A | 静默（去重） |
| 4 换卡B（GA777→C03） | 一条新增播报 |
| 5 重复卡B | 静默 |
| 6 拿开卡回空场 | 静默 |

**2026-09 实测：6/6 通过；RSS 峰值 16.7 MB；首见→开口 1786~2083 ms（确认 2 轮，另加相机/网络/轮询约 0.3~0.8s）。**
阶段帧由 `tests/make_rs_phases.py` 在本机生成（phase_0..5.jpg）。

## Web 配置台（现场调试人员用）
**浏览器打开 `http://192.168.8.100:8080`** 即可配置，无需 SSH。`deploy/config_web.py`（Python 标准库，零 pip 依赖）+ `deploy/announce-web.service`（systemd 常驻，开机自启）。

| 功能 | 说明 |
|---|---|
| 摄像头源 | simulate / **USB 自动发现** / RTSP 地址（含 IP 账号密码），下拉切换 |
| **USB 自动发现** | 点「🔍 自动发现」或打开页面即自动列出可用 USB 摄像头（`videoN · 型号 · 物理口`），无需手填设备号 |
| **抓帧预览** | 点一下抓一帧当前画面（USB 走主程序落盘、RTSP 自己拉流），实测 **0.2~0.5 秒**出图。**默认就用它**，不占 CPU |
| **实时预览（默认关闭，需手动勾选）** | 只有现场对准摄像头/框 ROI 时才临时开，可选 **2 / 5 / 10 秒一帧**；拖动 ROI 时自动暂停（不打断框选）、页面切到后台自动停、连续 3 次失败自动退出。**小主机只有 4 核，别常开** —— 见下文"实时预览为什么默认关" |
| **USB 预览不抢设备** | 主程序独占 `/dev/video0`，预览改为「请主程序落盘最新帧」（`_live/req` → `_live/latest.jpg`），实测 **0.2~0.5 秒**出图 |
| **🩺 测试识别** | 用当前 ROI 立刻抓帧识别一遍，显示**耗时 + 原文 + 命中号码**——现场调 ROI 的闭环工具 |
| **🔊 测试播报** | 不等识别触发，用当前配置念一句样例（GA200→08号窗口），**按 repeat 配置重复播放**（现为 2 遍），并回报实际引擎（常驻服务 / CLI / espeak） |
| **🔧 音量修复** | 一键把声卡 Master/PCM 拉满并解除静音（现场"没声音"九成是这里被静音/归零） |
| ROI 拖框 | 预览图上**按住鼠标拖矩形**即选识别框，坐标自动换算（也可手填） |
| 参数编辑 | 检测（帧差阈值/确认轮数/观察期…）、播报（开关/次数/语速/逐字念）、OCR psm |
| 保存并重启 | 校验 → 原子写 config.json → 自动重启 announce-rs，非法参数直接拒收 |
| 状态与日志 | 实时显示播报服务 active/PID/内存 + announce_log.csv 尾部 |
| **日志与磁盘** | 显示 CSV 大小/条数、存证张数、归档数、journal 占用、磁盘余量、**下次自动清理时间**；可一键「归档并清空播报日志」「清理存证截图」（详见下文「日志与磁盘：自动清理」） |
| **主机网络设置** | 改小主机 **IP/掩码/网关/DNS**、恢复出厂 IP(192.168.31.1)，**90 秒不确认自动回滚，不会变砖** |

> **保存语义（重要）**：页面不编辑的字段（`ocr.pattern` 正则模板、`ocr.bin/dpi`、`tts.engine/template/gap_sec/aplay_device/piper_*`、`detect.min_interval_sec` 等）保存时**一律沿用 config.json 现有值**，不会被重置成默认值。
> OCR 区会显示当前模板条数（如「正则模板 3 条」）；模板为 0 条时保存会被拒绝并提示。

### ROI 一定要"小而准"（否则播报严重滞后）
`tesseract` 的耗时**几乎正比于 ROI 像素数**，而主循环每轮都要跑一次 OCR：

| ROI | 像素 | 实测单次识别 |
|---|---|---|
| 整屏 `1787×1037`（框住全画面，含大量文字） | 1.85 M | **20~40 秒**（日志实测：主循环 41 秒才转一圈） |
| 只框号码行 `600×140` | 84 k | **0.7~1.2 秒** |

ROI 太大时：①识别慢 → 帧差检测形同虚设（40 秒才看一帧，屏幕跳号会被漏掉）；②播报延迟几十秒。
**正确做法**：拖框只框住"当前办理"那一行号码（约 800×120 量级），然后用「🩺 测试识别」确认：耗时应在 ~1 秒、并能在"命中号码"里看到正确结果。

#### ⚠️ 识别播不出来的连环坑（2026-09-24 实测复盘，已修）
现象：画面明明有号码，却一直不播报；日志里 `[监测]` 显示主循环 **7~57 秒才转一圈**（平均 24.5s）。
排查出的完整因果链（每一环都有实测数据）：

1. **服务里的 tesseract 比单独跑慢 10 倍**：同一张 ROI 图，SSH 里单独跑 **1.5s**；服务内却烧 **15s CPU**（`/proc` 采样：CPU/墙钟=1.01，是真多干了活，不是被卡住）。
2. 诱因是 **cgroup CPUQuota=150%**：`/sys/fs/cgroup/.../cpu.stat` 显示 **98.6% 的调度周期被限流**（`nr_throttled 4814 / nr_periods 4880`）。这个 cgroup 里住着主程序+ffmpeg+tesseract，配额边缘反复排队。→ **CPUQuota 放宽到 300%**。
3. tesseract 多线程在配额边缘会**线程间空转抢占**：加 `OMP_THREAD_LIMIT=1` 后单线程反而更快更稳（实测 1.36s vs 1.47s），且彻底免疫限流放大。→ **ocr.rs 固定单线程**。
4. **psm11 对反光/运动模糊帧会病态慢**（部分帧 10~20s）：加了 **10 秒超时**，超时按无候选处理，绝不让一张坏帧拖死主循环。
5. **致命一击：确认窗口 TTL=4s < 圈速 24s** —— 候选每轮都被 `confirm.retain` 清掉，**永远凑不齐 stable_frames=2 轮 → 永远不播报**。→ TTL 自适应：`max(配置值, 3×本轮OCR耗时)`。

配套改进：
- **TTS 预热**：第一轮识别到候选就后台向常驻合成服务发合成请求（日志 `[预热]`），确认通过真正播报时**命中缓存（毫秒级）**——把 1.8s 合成从关键路径整体藏掉。
- **兜底复查空转退避**：连续无候选时 3s→6s→…→30s 自动拉长，有候选立即复位。
- **[监测] 行自带 OCR 账本**：`OCR=N次 均匀X.Xs 最长X.Xs`，远程一眼看出 OCR 是不是又病了。

排障口诀：**主循环圈速（看相邻 `[监测]` 时间戳）> 确认 TTL → 必然不播报**。修法优先级：①看 OCR 均耗是否异常 ②查 `cpu.stat` 限流率 ③ROI 是否框进了反光/无关区域。

#### ⚠️ 续篇：修完性能还是不播报（2026-09-24 下午实测复盘，已修）
上面五环修完、圈速恢复 0.4s/圈后，生产**依然 0 播报**。继续排查出的三环：

6. **致命 bug：tesseract 结果被 mpsc 通道"吃掉"** —— `run_tesseract` 里 stdout 排空线程
   `tx.send()` 只能成功一次，而主线程先"排空"再"取结果" recv 了两次：mpsc 的 recv 是
   **消费性的**，第一次就把结果取走了，第二次永远超时返回空串 → **所有 OCR 结果都是空**。
   → 修为只 recv 一次。这个 bug 沙盒自检（CSV 空全失败）抓出来了，性能日志完全看不出来。
7. **真实画面 OCR 全乱码（沙盒测不出来的盲区）**：六阶段自检用的是高对比度干净测试卡，
   6/6 通过；但摄像头实拍"白字深蓝底+小字+糊"，tesseract 原图输出全是
   `777双co三霄吾` 级别的乱码。→ **增强识别链**（`detect::enhance_for_ocr`）：
   **2x 放大 → 取红通道（白字三通道都高、深蓝底只有红通道最低，对比拉满）→ Otsu 自动
   阈值二值化（必须有！没有这步"号窗口"糊成"写窗"正则套不上）→ 反相成白底黑字**，
   原图 → 增强图 psm11 → 增强图 psm6 三段先中先用。实测同一张真实 ROI 图，
   原图全乱码、增强后 `6A777到003号窗口` 级别可解析。
8. **解析端两条稳定变异清洗**（`ocr.rs`）：①号码前缀被读成形近数字
   `6A777→GA777`、`64999→GA999`（仅当"映射字符+3~4位数字+到/至"才替换，普通数字免疫）；
   ②窗口号误插 0：`到0C01→到C01`（0 后跟字母全删）、`到003号→到03号`（C 被读成 0，丢一个）。

#### ⚠️ 又一环：服务重启后"取帧失败"刷屏（已修）
现象：部署重启后日志刷 `[错误] 取帧失败，1秒后重试`，`ps` 里挂着一个 `[ffmpeg] <defunct>` 僵尸。
原因：上一个实例被 stop 时其 ffmpeg 子进程短暂残占 `/dev/video0`，新实例的 ffmpeg 首次
打开设备失败即退；原实现"管道读取结束 → dead 永久置位"，**抓帧线程没有任何自愈能力**，
一次失败瘫到人工重启。→ 重写 `PipeGrabber`：读断/启动失败 → kill+wait 收尸（不留僵尸）
→ 3s 后自动重新拉起 ffmpeg，直到 Drop 为止；Drop 用 stop 标志 + kill 收尾，不再死锁。

**验收数据（生产实况）**：`announce_log.csv` 出现 Rust 版上线以来**第一条 OK**——
`13:30:22, GA777, 03, …, OK`，`[播报] 请G A 七 七 七到零 三号窗口办理`，首见→开口 4.4s
（含增强链 OCR 5.9s 一轮 + 2 轮确认）。

#### 页面预览花屏/只有一条横带的坑（已修）
现象：配置台的画面预览像"被压扁的一条"，或大面积发白 —— **不是摄像头坏**，是预览帧被截断了：
主程序 `create("latest.jpg")` 的瞬间文件就已可见，配置台轮询到 `exists()` 就读，90KB 的 JPEG 才写到 8KB。
修法（两层都上了保险）：
- 主程序（`capture.rs serve_preview`）：先写 `_live/latest.jpg.tmp` 再 `rename` **原子替换**；
- 配置台（`usb_preview_via_service`）：**指纹轮询** —— 记下 `latest.jpg` 的 `(mtime_ns, size)`，主程序原子替换后必然变化，看到变化即取；再用 JPEG 结束标记 `FFD9` 兜底校验（兼容旧的直接 create 版主程序）。
排障时看下载字节数即可判断：`curl -s http://.../api/snapshot -o x.jpg -w '%{size_download}'`，健康值 ≈ 90~110KB（1080p），只有 8192 就是中了这个问题。
另注意：预览里一大片白也可能只是**摄像头真对着白墙**（没对准叫号屏）——先看图内容，再怀疑程序。

#### "抓帧预览抓的不是实时的"（已修，2026-09-24）
领导现场反馈：点「抓帧预览」出来的画面不是当前画面。**实测取证后确认后端没问题**：
抓帧管道实测 23~30 fps（日志 `[抓帧] 管道已读 N 帧` 持续增长），连续抓帧的 MD5 互不相同 —— 每次都是新帧。
真正的三个原因，都已修：

| 原因 | 证据 | 修法 |
|---|---|---|
| **浏览器缓存同一 URL** | `/api/snapshot?src=0&w=1920&h=1080` 这串 URL 恒定不变，而响应头只有 `Content-Type/Content-Length`，**没有 Cache-Control** | 响应加 `Cache-Control: no-store, no-cache, must-revalidate, max-age=0` + `Pragma` + `Expires: 0`；前端 URL 再加 `&_t=Date.now()` 并 `{cache:'no-store'}` |
| **取帧白等 0.3 秒** | 旧逻辑"删文件→等重新出现→等大小两次采样相同"，固定开销就是 `0.15×2+0.1×N` 秒，单帧 0.9s | 改指纹轮询（`mtime_ns+size` 变化即取），单帧降到 **0.5s** |
| **根本不自动刷新** | 前端只有 `onclick=snap()`，点一次出一张静态图，页面不会自己更新 | 加「实时预览」开关 + 打开页面抓 1 张静帧（**默认不连续刷帧**，见下节） |

> 顺带一个容易被误判为 bug 的现象：USB 摄像头对着**静止画面**时，相邻帧可能**逐字节相同**（MJPG 编码确定性输出），抓两次 MD5 一样 —— 这时的"画面不动"是场景真没动，不是程序卡住。**判据是日志里的帧号是否还在增长**：
> `journalctl -u announce-rs | grep '管道已读' | tail -3`
> 帧号持续增长 = 管道正常，画面内容静止是场景问题；帧号停住才是真的卡死（多半摄像头掉线）。

#### 实时预览为什么默认关闭（2026-09-24 现场反馈"开着就卡"）
背景：为修"抓帧预览不是实时的"，一开始让页面打开即**自动 1 秒 1 帧**刷新。领导现场反馈这样小主机会卡 —— 立刻改为**默认关闭 + 缩图**：

| 项 | 之前 | 现在 |
|---|---|---|
| 默认状态 | 打开页面即自动开启，1 秒 1 帧 | **默认关闭**；打开页面只抓 **1 张静帧**（画面不为空、可直接拖 ROI），要连续画面自己勾 |
| 刷新间隔 | 固定 1 秒 | 可选 **2 / 5 / 10 秒**（默认 5 秒） |
| 后台行为 | 一直请求 | 页面切后台/关闭**立即停**；拖动 ROI 自动暂停；连续 3 次失败自动退出 |
| 单帧开销 | 1920×1080 全尺寸编码 + 6MB 整帧克隆 | 小主机侧先缩到 **960×540** 再编码（`capture.rs` 的 `PREVIEW_DIV`）：像素量 1/4、单帧 ≈90KB → **≈34KB**、出图 0.9s → **0.2~0.5s** |

实测（`bash deploy/_verify_light.sh`，带静置基线 A/B）：

| 指标 | 实测 |
|---|---|
| 单帧端到端 / 尺寸 | **0.27~0.48s** / 33856 字节 / 960×540 |
| 连发 20 帧（7.5s 墙钟）进程 CPU | 2.55s ≈ 同长度**静置基线** 2.45s → 预览增量在噪声内 |
| **2 秒 1 帧刷 40 秒** | 进程 CPU **29.4% 单核**，静置基线 **32.8%** → **预览净成本 ≈ 0** |
| 内存 | 23~25MB，40 秒内无增长 |
| 参照 | 主循环本身（1080p 帧差 + OCR 调度）就占 **~33% 单核** |

> ⚠️ **缩图带来的隐藏坑（已处理）**：ROI 坐标一直是**采集分辨率**口径（当前 1920×1080，值 `[820,740,401,180]`），而预览图现在是 960×540。前端坐标换算必须用页面上的「分辨率」输入框（`capW()`），**不能**再用 `img.naturalWidth` —— 否则重拖 ROI 得到的是只有一半的坐标，主程序裁错区域、识别必然失败。
> 通用教训：**只要预览做过任何缩放/裁切，坐标换算就必须锚定"业务坐标系"，而不是"显示图像的像素数"。**

## 语音播报（中文女声）
现场播报用 **piper 神经语音 + 中文女声「华言 medium」**（`deploy/install_piper_zh.sh` 一键装），比 espeak-ng 自然得多。

- **为什么不用 espeak-ng**：靶机 espeak-ng 1.51 的中文音色官方名是 `Chinese (Mandarin, latin_as_English)` —— 数字/字母按**英语**念（"200"读成 two hundred），音色也是几十年前的共振峰合成，现场听感像"机器人在讲英语"。
- piper 安装（需外网：TUNA PyPI + hf-mirror.com，GitHub 不通也可装）：`sudo bash deploy/install_piper_zh.sh`
- 模型：`zh_CN-huayan-medium.onnx`（63MB，离线推理，不依赖外网）
- config.json 对应字段：`tts.engine="piper"`、`tts.piper_bin`、`tts.piper_model`、`tts.piper_lib_dir=""`、`tts.piper_url`
- 引擎优先级：**常驻服务 → piper CLI → espeak-ng**（中文音色自动探测 `cmn`→`zh`）
- **现场"没声音"排查顺序**：①页面点「🔧 音量修复」（Master 静音/归零是头号原因）②点「🔊 测试播报」听 ③确认音箱插在 3.5mm 模拟口（默认输出为 card0 `ALC662 Analog`）
- 静音状态已 `alsactl store` 持久化，重启不丢。

### 常驻合成服务（`announce-piper.service`）—— 为什么需要它

piper 是命令行程序，**每念一句都要重新加载 63MB 模型**，实测单次 12.6~13.6 秒（且与热冷无关，是 onnxruntime 图初始化开销）。现场"识别到号码 → 等 13 秒才出声"不可接受。

因此增加一个常驻合成服务 `deploy/tts_server.py`（纯标准库 HTTP，无 flask 依赖）：进程内 `PiperVoice.load()` 一次，之后每次请求直接推理。

| 方式 | 每次出声 | 常驻内存 |
|---|---|---|
| piper CLI（每次重载模型） | 12.6~13.6 s | 0（峰值 199MB） |
| 常驻服务 + medium（**旧配置**：`CPUQuota=150%` + `Nice=5`） | 4.6~5.3 s | 约 233MB |
| **常驻服务 + medium（现用）** | **1.74~1.85 s** | **约 200MB** |
| 常驻服务 + x_low 小模型（同优先级实测，**不值得换**，见下） | 1.53~1.87 s | 约 158MB |
| espeak-ng（不要音质时可切） | 0.07 s | ~0 |

> **现场口径**：页面上点「🔊 测试播报」到**第一声**约 **2.0 秒**（合成 1.8s + aplay 启动 0.2s）。
> 点一次会按 `tts.repeat`（现为 2）连念 2 遍，所以接口整体约 8.8 秒才返回，属正常。

#### ⚠️ 关键坑：systemd 的 `CPUQuota` 会把神经 TTS 拖慢一倍

**现象**：裸进程跑 piper 合成只要 2.6s，走 `announce-piper.service` 却要 **4.6~5.3s**。

**根因**（cgroup v2 实测取证）：原单元写的是 `CPUQuota=150%`（约 1.5 核）+ `Nice=5`。而 onnxruntime 推理是多线程的，被配额掐住后
`/sys/fs/cgroup/system.slice/announce-piper.service/cpu.stat` 显示：合成 4 次就 `nr_throttled` 从 749 → 946，**`throttled_usec` 累计被掐掉 30 秒 CPU 时间**。
再叠加当时 `tesseract`（OCR）常态吃 130%+ CPU、且两者 `Nice` 同为 5 抢不到，于是"限流 + 抢不到核"双重恶化。

**修法**（`deploy/announce-piper.service`，已生效）：

```ini
Nice=0                # tesseract 继承 announce-rs 的 Nice=5, 语音不再低人一等
CPUWeight=800         # CPU 争抢时 800 vs announce-rs 的 50, 语音优先
CPUQuota=350%         # 仍留保险, 最多吃 3.5 核, 不至于占死整机
Environment=OMP_NUM_THREADS=3   # 4 核里留 1 核给摄像头抓帧/OCR
```

改完 `nr_throttled` 基本不再增长，合成从 4.6~5.3s → **1.74~1.85s**。
**教训**：给神经推理类常驻服务设 CPU 配额要按"突发任务"算，不能按"持续负载"算 —— 合成是偶发几秒的活，配额是全天候的锁。
一键重装/回滚：`python deploy/_piper_tune.py`（回滚加 `--rollback`）；模型基准：`python deploy/_ttsbench.py`；端到端验证：`python deploy/_verify_tts.py`；缓存与健康快检：`python deploy/_final_check.py`。

预览相关：一键部署+自验（含防缓存头/取帧耗时/页面含"实时预览"）：`python deploy/_deploy_live_preview.py`；预览链路诊断（帧率、落盘时刻、逐字节比对）：`bash deploy/_diag_preview.sh`（远程跑用 `python deploy/remote_run.py`）；实时预览压测（30 帧成功率/耗时/CPU/内存）：`bash deploy/_stress_live.sh`。
预览轻量化（默认关闭 + 缩图）一键部署+实测：`python deploy/_deploy_light.py`；**预览 CPU 成本带基线实测（静置基线 A/B + 单帧成本 + 40 秒持续刷帧）**：`bash deploy/_verify_light.sh`。

#### 合成缓存（`tts_server.py` 内置）
服务内建 LRU 缓存（`text → wav` 字节，上限 48MB，够存数百句），命中直接返回，**耗时从 1.8s 降到 3~7 毫秒**。
现场收益：同一号码被重复检测、现场连点「测试播报」、`repeat` 多遍都不用重算。统计见 `GET /health`（`hits`/`misses`/`synth_avg_sec`），清空用 `POST /cache/clear`（仅本机）。

#### 为什么没有选 x_low 小模型
20MB 的 `zh_CN-huayan-x_low.onnx` 单独压测确实快一倍（RTF 0.55 vs 0.67），但**在解除限流后同优先级实跑，差距只剩 6%**（1.69s vs 1.79s，属噪声范围），却要付出 16kHz 采样率 + 音色明显更机械的代价 —— 不划算，保持 medium。
（真正拖慢的从来不是模型大小，是 CPU 配额。）

#### 降级安全
主程序优先 POST 到 `tts.piper_url`；服务不可用（未启动/超时/返回空）会**自动回退 CLI 合成**并在日志打印 `[TTS] 常驻服务不可用(...), 回退 CLI 合成`，最差退到 13 秒，绝不哑掉。
合成服务与播报、配置台一起纳入工作时间定时启停（18:00 停 / 08:00 起）。
若要换模型：改 `config.json` 的 `tts.piper_model` 后，**必须同步改 `announce-piper.service` 的 `--model` 并重启该服务**（模型是服务启动参数，不是运行期读取）。

### USB 自动发现实现说明（`GET /api/devices`）
纯 Python `fcntl.ioctl(VIDIOC_QUERYCAP)` 枚举 `/dev/video*`，**零外部依赖**（不需要 v4l-utils）：

- 以 `device_caps` 判定采集能力（capa 置 `DEVICE_CAPS` 位时），**只列真采集节点**——UVC 摄像头通常同时暴露 `video0`(采集) 与 `video1`(metadata 从节点)，后者选中必失败；
- 掩码用 `V4L2_CAP_VIDEO_CAPTURE = 0x00000001`（`0x04000000` 是 STREAMING，写成它会把从节点也列出来）；
- 附带 `bus_info`(如 `usb-0000:00:14.0-4.3`) 便于**多台同型号时按物理口区分**；
- 无权限时返回可读提示（服务账号需在 `video` 组；本机 debian 已在组内）；
- 下拉无果时可退回手填设备号（保留 `usb_idx` 输入框）。

## 主机改 IP 的保护机制（90 秒回滚）
改 IP 走 `deploy/announce-net-apply.sh`（root 专用脚本，sudoers 白名单）：备份原配置 → 写静态 IP → 重启网络 → 挂 90 秒回滚定时器。
**90 秒内用新 IP 重开配置台并点「确认保留」**即固化；不确认则自动回滚旧 IP——改错网段/输错数字都不会失联变砖。
- ⚠ 出厂 IP `192.168.31.1` 仅供**网线直连调试**场景（笔记本直插主机）；**有路由器的现场该地址多为网关，会冲突**，请改用网段内空闲地址（DHCP 保留或网管分配）。
- 重复修改保护：上一次修改还在 90 秒确认期内时，拒绝再次 apply（防止把坏配置备份成回滚点）。
- 参数硬校验（脚本白名单只收数字/点/空格），杜绝向 interfaces 文件注入。

安全模型：**仅限内网使用，页面无认证**；重启主服务走 sudoers 窄授权
（`/etc/sudoers.d/announce-web`：debian 仅免密 `systemctl restart announce-rs`，无其他 root 权限）。
注意：RTSP 密码含 `@ : # ?` 等符号需 URL 转义（如 `@` → `%40`）。

## 工作时间自动启停（18:00 停 / 次日 08:00 起）
播报服务、语音合成与配置台仅在 **每日 08:00~18:00** 运行（`deploy/announce-worktime-*.service/.timer` 四件套，systemd 原生定时器）：

- **18:00** 自动 `stop announce-rs announce-web announce-piper`（非工作时间静默省电，也不会有任何误播）；
- **08:00** 自动拉起三个服务；
- `Persistent=true`：小主机若在触发点断电/关机，开机后会补执行错过的那次启停；
- 验证：`systemctl list-timers 'announce-worktime*'`；临时要全天跑可 `systemctl stop announce-worktime-stop.timer`（调试日建议如此，收工记得再开）。

## 日志与磁盘：自动清理（每日 03:30）

小主机是 7×24 跑的，日志/存证只增不减，迟早写满磁盘、也白耗 eMMC 写入寿命。**这套清理装好之后不用管**，共三层：

| 层 | 对象 | 策略 |
|---|---|---|
| ① journald 硬上限 | systemd journal（主程序/配置台的全部 stdout） | `/etc/systemd/journald.conf.d/99-announce.conf`：`SystemMaxUse=64M`、单文件 16M、最多 5 个、`MaxRetentionSec=1month`。**超出自动丢弃最旧的**，永远涨不上去 |
| ② 主程序日志降噪 | 主程序自己往 journal 打的字 | 抓帧心跳 每 60 帧 → **每 10 分钟**一条；预览成功日志 每次请求 → **每 120 秒**最多一条（失败仍逐条留痕）。journal 写入量从 **40320 行/天 → 约 1500 行/天**（降 96%） |
| ③ 业务文件轮转 | `announce_log.csv`、`_live/trig_*.jpg`、归档、部署备份 | `announce-cleanup.timer` 每日 **03:30** 执行，`Persistent=true`（断电错过会补跑） |

③ 的具体规则（**全部只在本项目目录内、只用白名单文件名，绝不 `rm -rf`、绝不通配符横扫**）：

| 文件 | 规则 |
|---|---|
| `announce_log.csv` | 超 **8MB** 才轮转到 `logs_archive/announce_log_<时间戳>.csv`，**只轮转不删除**；保留最近 **6** 个归档 |
| `_live/trig_*.jpg`（播报存证） | 主程序自身上限 20 张（文件名固定，超出会覆盖）；清理任务再删 **7 天前**的 |
| `_live/latest.jpg.tmp`、`_live/req` | 清理中断残留的临时文件、超过 1 小时没被主程序消费的陈旧请求 |
| `*.bak.*`（部署备份） | 保留 **30 天** |
| 磁盘可用 < 500MB | 自动**激进清理**（删全部存证图与归档、journal 收缩到 32MB）并把经过写进 journal |

### 在页面上看 / 手动清理
配置台底部「**日志与磁盘**」面板会显示：CSV 大小与条数（含首末时间）、存证张数、归档个数、journal 占用、磁盘已用/可用、**下次自动清理时间**。两个按钮：

- 「📦 **归档并清空播报日志**」—— 把 CSV 搬到 `logs_archive/`（历史不丢）后重新开始记；
- 「🧹 **清理存证截图**」—— 清掉 `trig_*.jpg`（下次触发播报会重新生成）。

> 这两个按钮只用 debian 自身权限（本机 debian 不在 sudoers，所以不走 sudo）。含 journal 收缩的**完整清理**交给每日 timer 以 root 执行。

### 手动命令（SSH）
```bash
systemctl list-timers announce-cleanup.timer    # 下次何时清理
systemctl start announce-cleanup.service        # 立刻完整清理一次(需 root)
journalctl -u announce-cleanup -n 30            # 看上次清理做了什么
curl -s localhost:8080/api/logs | head -c 400   # JSON 形式看占用
```

### 想调参数
改 `deploy/announce-cleanup.sh` 顶部那几个变量（`CSV_MAX` / `ARCHIVE_KEEP` / `TRIG_KEEP_DAYS` / `BAK_KEEP_DAYS` / `TIGHT_KB`），重新上传后 `systemctl restart announce-cleanup.timer`。
journal 上限改 `/etc/systemd/journald.conf.d/99-announce.conf` —— **删掉这个文件即完全恢复系统默认**（不用碰主配置）。
