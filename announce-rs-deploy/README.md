# announce-rs 部署说明（i3-7100 / 4GB / Debian 13）

项目：<https://github.com/qianqianjie-999/announcer-ocr>（叫号屏摄像头 OCR 语音播报，Rust）
方案：**裸机部署，不使用 Docker**。单二进制 + 一个 config.json，外部依赖只有系统包。

---

## 一、结论：这台机器能跑，而且余量不小

### 为什么兼容性没问题

| 检查项 | 要求 | i3-7100 实际情况 |
|---|---|---|
| 架构 | amd64 | ✅ Kaby Lake，标准 x86-64 |
| 指令集 | 无特殊要求 | ✅ 支持到 x86-64-v3（连 RHEL 10 的基线都满足） |
| 内核 | 6.12 LTS（Debian 13 自带） | ✅ 完全支持 |
| 依赖库 | 无 C 绑定、无 OpenCV、无 ONNX Runtime | ✅ 只有 serde / image / regex / chrono 四个纯 Rust 依赖 |

`Cargo.toml` 的依赖清单极短，编译产物是**静态单文件二进制**，没有 Python 运行时依赖（Web 配置台用的是 Python 标准库，零 pip 包）。这类程序对硬件的要求几乎只取决于外部命令行工具。

### 4GB 内存的实际占用预算

| 组件 | 常驻内存 | 说明 |
|---|---|---|
| Debian 13 最小系统（无桌面） | 150 – 250 MB | |
| `announce-rs` 主进程 | 30 – 80 MB | Rust 静态二进制，非常轻 |
| `ffmpeg` 拉 RTSP 子码流 | 60 – 120 MB | 640×360 软解，负载很低 |
| `tesseract`（按需派生子进程） | 120 – 250 MB / 次 | 只在画面变化时启动，非常驻 |
| `piper` 合成（CLI 按需调用） | 90 – 150 MB / 次 | 非常驻，每次加载约 0.25s；不用 Python 常驻服务 |
| Python Web 配置台 | 20 – 40 MB | 可随时关掉 |
| **合计常驻** | **约 0.7 – 1.1 GB** | |
| **峰值（tesseract 运行时）** | **约 1.3 GB** | |

**结论：4GB 有 2.5GB 以上空闲，跑得动而且还能顺带跑别的服务。** 换 piper 真人音色也不需要加内存；但如果后面再想在同一台机器上叠数据库之类的重负载，就该考虑加内存了。

### CPU 负载

- 主循环 3 fps，帧差检测是灰度缩放后的轻量运算，几乎不吃 CPU；
- **屏面不动不触发 OCR**，这是这个项目功耗低的根本原因；
- tesseract 单次识别在 i3-7100 上属于几百毫秒级，因为 ROI 裁出来只有几百×几百像素；
- 唯一要守住的纪律：**必须设 `OMP_THREAD_LIMIT=1`**。tesseract 默认开满核并行，在 2 核 4 线程上会把主循环的帧差检测饿死，反而整体更慢——这是项目方在靶机上实测出来的结论，部署脚本里已经写进 systemd 单元了。

---

## 二、执行部署

```bash
# 1. 脚本从 Windows 拷到 Debian 后，先修行尾
sed -i 's/\r$//' deploy.sh

# 2. 先体检，不改动系统（强烈建议先跑这个）
sudo bash deploy.sh --doctor

# 3. 正式部署
sudo bash deploy.sh --with-swap                 # 模拟模式 + 补 2GB swap，用于冒烟
sudo bash deploy.sh --with-swap --with-piper    # 加上中文真人音色
sudo bash deploy.sh --rtsp 'rtsp://user:pass@192.168.1.64:554/stream2' --with-piper
```

脚本是幂等的，重复执行不会重复破坏已有配置。

### Web 配置台登录

首次部署会生成 `/etc/announce-rs-web.env`，终端最后会输出 Web 配置台的用户名和口令。
重复执行不会覆盖已有口令。忘记口令时可先备份配置，再删除该文件后重跑部署脚本；
也可以直接编辑其中的 `ANNOUNCE_WEB_PASSWORD`，然后执行：

```bash
sudo systemctl restart announce-web
```

HTTP Basic 只负责访问控制，不提供传输加密。跨不可信网络访问时必须经过 HTTPS
反向代理；配置台仍只应开放给运维网段。

---

## 三、三个真正的坑（不在软件本身，在环境）

### 坑 1：服务器没有声卡，aplay 出不了声 ★最容易被忽略

程序播报链路的最后一环是 `aplay` 往 ALSA 设备写 PCM。台式/工控机器如果没有板载声卡，或声卡被静音，会表现为「日志里有 `[播报]`，但现场没声音」——看起来像程序 bug，其实是硬件。

```bash
aplay -l                       # 列出所有声卡，没有输出就是没声卡
speaker-test -t sine -l 1      # 有卡时试听 1 秒
```

**即使 `aplay -l` 有声卡，新装 Debian 的 `Master` 通道默认是静音 + 音量 0%**，播放会"成功返回"但完全没声音。这几乎是必踩的一条：

```bash
amixer -c 0 scontrols                    # 先看有哪些控件
amixer -c 0 get Master | grep Mono       # 看到 [0%] [-65.25dB] [off] 即为静音
amixer -c 0 set Master unmute 70%        # 解静音 + 音量 70%（90% 以上会削波失真）
amixer -c 0 set Front unmute 70%
amixer -c 0 set 'Auto-Mute Mode' Disabled
alsactl store                            # 持久化，重启后不再被复位
```

Web 配置台也内置了 `/api/audio_fix` 一键修复入口，不用记上面这些命令。

处置顺序：
1. **首选**：插一个 USB 声卡（十几到几十块），接有源音箱。`deploy.sh` 已把 `announce` 用户加入 `audio` 组。
2. 声卡存在但设备号不是默认的：在 config 里指定 `"aplay_device": "plughw:1,0"`（编号来自 `aplay -l`）。
3. 完全无声卡的机器：用 `snd-dummy` 内核模块造一个虚拟声卡，再配合网络音频（snapcast / squeezelite）把声音推到现场音箱。

### 坑 1.5：espeak 读拉丁字母"像老外"

`espeak-ng` 用 `cmn`（普通话）音色时，对拉丁字母**设计上就按英文读**——直接合成 `请GA200到C08号窗口办理`，`GA200` 会读成英文的 "G A two hundred"，听起来就是"老外腔"。

正确姿势是配置里的 **`"spell_number": true`**：它先把号码逐字符拆成 `G A 二 零 零`、把窗口号拆成 `C 零 八`，再交给中文音色合成。**这条路径才是对的**，别关掉它。想更接近真人音色需上 piper（见 `--with-piper`）。

### 坑 2：ROI 标定不到位，识别会全乱

项目文档里反复强调的一条：**ROI 必须收紧到叫号屏区域本身**，全画幅模式下识别文本会被背景杂物污染成"满屏噪声"。现场还有三类具体陷阱：

- **滚动字幕跑马灯**：会让帧差持续触发 OCR，白烧 CPU。ROI 只框上半"当前办理"两行，把滚动区排除；
- **玻璃反光**：摄像头别正对反射角，偏角度安装；
- **双屏同框**：视野里出现第二块屏时必须裁掉。

正确做法：部署完先启服务 → `systemctl start announce-web` → 浏览器开 `http://<机器IP>:8080` → 拖框标定 → 保存（会自动重启主服务）→ 点页面的「试识别」确认框内读到的内容正确。标定完用 `_live/` 里的存证帧复核框内确实没有杂物。

两个已知的坐标约定，别被吓到：
- **ROI 用的是"配置采集分辨率"的坐标（如 1920×1080），不是预览图坐标。** 配置台取到的预览帧是主程序降采样过的（`PREVIEW_DIV=2`，即 960×540），前端会自动按比例换算，你不用手动折算。
- **主程序独占 `/dev/video0`。** 服务在跑时，配置台是通过与主程序握手拿预览帧的，不会抢占设备；服务停掉时才直接开设备抓帧。所以别在服务运行中用 `ffmpeg -i /dev/video0` 手测，会报 `Device or resource busy`——那是正常的。

### 坑 3：用主码流会白白吃掉 CPU

RTSP 摄像头务必取**子码流**，并把 `capture_width/height` 同步改成子码流的实际分辨率。分辨率一变，旧 ROI 可能出界——程序会跳帧并告警一次（不会崩），重新标定即可。

i3-7100 实测（大华 4MP，主码流 2688×1520 / 子码流 704×576）：

| 码流 | 分辨率 | 整机 CPU 占用 | 内存 |
|---|---|---|---|
| 主码流 subtype=0 | 2688×1520 | **约 45%**（ffmpeg 独占约 1.2 核） | ffmpeg RSS ~460MB |
| 子码流 subtype=1 | 704×576 | **约 6%** | ffmpeg RSS ~90MB |

差距 7 倍的根因不只是 H.264 解码量，更是**原始帧走管道的吞吐**：2688×1520 裸帧约 12MB/帧，管道流量 300MB/s 级别；704×576 只有 0.6MB/帧。

选型建议：**默认子码流**（24 小时开机省电、留足余量）。只有当窗口号在画面里小到 704×576 下 OCR 读不准时，才切主码流——那是一台专用机，45% 是可以接受但要心里有数的代价。折中方案是把摄像头**推近**或在摄像头 Web 端把子码流分辨率调高到 1280×720。

注意：Dahua 的 RTSP 地址是 `rtsp://user:pass@IP:554/cam/realmonitor?channel=1&subtype=N`（`subtype=0` 主 / `1` 子），不是 Hikvision 的 `Streaming/Channels` 格式。部分机型 `subtype=2` 不存在，会返回 404。

---

## 四、上线验收清单

按顺序做，每步都有明确的通过标准：

| # | 动作 | 通过标准 |
|---|---|---|
| 1 | `sudo -u announce /opt/announce-rs/announce-rs /opt/announce-rs/config.json` | simulate 模式下能读到 `sim_frame.jpg` 并打印 `[播报]` |
| 2 | `tesseract --list-langs \| grep chi_sim` | 输出 `chi_sim` |
| 3 | `aplay -l` | 列出至少一块声卡；`speaker-test` 能听到声音 |
| 4 | `ffprobe -rtsp_transport tcp -i '<url>'`（在 Debian 上执行） | 能列出流信息，说明同网段可达 |
| 5 | 切真实源后 `journalctl -u announce-rs -f` | 看到帧差触发 → OCR → 播报的完整链路日志，端到端延迟 1–3 秒 |
| 6 | 手动换一次屏面号码 | 2 秒内听到播报，`/opt/announce-rs/announce_log.csv` 多一行（Excel 可直接打开） |
| 7 | `systemctl is-enabled announce-rs` | 输出 `enabled`，断电重启后自动恢复 |
| 8 | 浏览器开 `http://<机器IP>:8080` 并点「试识别」 | 页面回到耗时 + 识别原文 + 命中号码；`systemctl is-active announce-web` 为 `active` |
| 9 | 配置台改个参数点保存 | 自动重启主服务，参数在新一轮日志里生效（验证 sudo 窄授权可用） |
| 10 | 配置台勾选「启用工作时间调度」并设起止时间 → 保存 | `systemctl list-timers 'announce-worktime-*'` 出现 start/stop 两个定时器，下次触发时间与设定一致 |

---

## 五、工作时间调度（Web 可配置）

到点自动启停，**非工作时间不采集、不识别、不播报** —— 既省 CPU，也避免非营业时段误播。

**怎么用**：浏览器开 `http://<机器IP>:8080` → 「工作时间调度」栏 → 勾选「启用」+ 填**开始/结束时间** → 保存。立即生效，无需登录机器。

**实现方式**（关键：**不改主程序**）：用 systemd timer 在 `work_start` 拉起、`work_end` 停止 `announce-rs`，共三个单元：

| 单元 | 作用 |
|---|---|
| `announce-worktime-start.timer/.service` | 每日 `work_start` 拉起播报服务 |
| `announce-worktime-stop.timer/.service` | 每日 `work_end` 停止播报服务 |
| `announce-worktime-boot.service` | 开机时按时间表校正一次状态（解决「非工作时段开机」不该跑的问题） |

三者由 `/usr/local/sbin/announce-schedule-apply` 统一生成——它读 `config.json` 的 `schedule` 段，写 timer 并**按当前时刻校正运行状态**，由 Web 台经 sudoers 窄授权调用（仅放行这一个脚本）。

**几个设计取舍**：

- **不改 Rust 源码**：`cfg.rs` 用 serde 反序列化且未开 `deny_unknown_fields`，多出的 `schedule` 段会被自动忽略，向后兼容。省掉一次重新编译，风险最低。
- **timer 用 `Persistent=false`**：错过的触发点不补跑。否则「机器在非工作时段开机」会把当天已错过的 `start` 补跑，反而在半夜把服务拉起来。开机状态由 `announce-worktime-boot` 统一校正。
- **支持跨午夜**：`22:00 ~ 06:00` 这类夜班时段同样正确（脚本内做区间环绕判断）。
- **默认不启用**：装完保持旧行为（全天运行），需要时在页面一键打开。

**排查命令**：

```bash
systemctl list-timers 'announce-worktime-*' --no-pager   # 看下次触发
systemctl is-active announce-rs                          # 当前是否在跑
sudo /usr/local/sbin/announce-schedule-apply             # 手动按时间表校正一次
```

---

## 六、调优速查

| 现象 | 处置 |
|---|---|
| 换屏了不触发 OCR | 先看 `_live/` 有没有新触发帧；没有则 `changed_ratio` 从 0.0015 调到 0.001 |
| 空场景反复 OCR 白烧 CPU | `changed_ratio` 调大；检查摄像头自动曝光是否剧烈漂移 |
| 识别结果全乱码 | 确认走了增强图（红通道 + Otsu）；确认 ROI 里没混入背景杂物 |
| 号码读成英语 | piper 场景设 `spell_number: true`；espeak 场景必须开（它对拉丁字母设计上按英语读）。**不要**指望 piper 的中文模型念孤立拉丁字母：它音素表里没有 `(en)`/`dZ`/`eI`，`C` 只会得到一个又短又弱的音（听感"没读"）。窗口字母念不念由 `tts.speak_window_letter` 决定（默认 false = 只念数字） |
| 首条播报要等十几秒 | piper CLI 每次要重载 onnx 模型。实测这台机器加载只要 0.25s，一般不用常驻服务；若确实要更快再配 `piper_url` 指向常驻服务 |
| 播报里窗口号少了个字母 | 先确认不是试播接口的默认参数（`/api/test_tts` 不传 `window` 时默认是 `"08"`，**不带字母前缀**）。传 `{"number":"GA200","window":"C08"}` 重试 |
| 同一号码重复播报（2~4 遍） | 见下面的「三个修复」，2026-10-13 已从根上解决 |
| 播报像英语（"诶提一七四"） | 这是号码被误读成 `AT174`：过渡帧把 `GA174` 多认一个字母 → 宽正则回溯切出 `AT174`。已由「稳帧再认 + 正则锚定」修复 |
| 存证图 `trig_*.jpg` 全是残影 | 不是抓拍频率太低，恰恰是"一动就抓"。已由「稳帧再认」修复：变化后等画面定型再存证 |
| 日志写不进去 | 文件被 Excel 占用，程序重试 3 次后丢该条，主流程不受影响，关掉占用者即可 |
| 重复启动直接退出 | 单实例锁占用回环端口 51888，属正常保护；要跑第二实例改 `lock_port` |

### 2026-10-13 的三个修复（本地补丁，升级上游时要重新打）

上游仓库里没有这三项，**用上游覆盖 `src/` 后必须重新打**：

1. **稳帧再认**（`main.rs` / `cfg.rs`）
   帧差判"有变化"后先静置 `detect.settle_sec`（默认 0.5s）**再** OCR，动画期间一律不认；
   静置到点用"定型帧"识别与存证。防"摄像头持续噪点导致永远等不到静置"的兜底：
   变化持续超过 `SETTLE_MAX_WAIT_SECS`（2s）就强制认一次。
2. **正则锚定 + 号码清洗**（`config.json` 的 `ocr.pattern` + `ocr.rs`）
   `(G[A-Z]{0,2}\d{2,6})[到至]([A-Z]{0,2}\d{1,3})`：号码必须以 `G` 开头，
   彻底堵死 `AT174` 这类回溯误切；`parse()` 新增 `only_digits()` 把数字段里
   夹进来的字母丢掉（`GAT174` → `GA174`），把误读**修回正确号码**而不是丢弃该轮。
3. **按号码去重**（`main.rs`）：`announced` 表键从 `(号码, 窗口)` 改为**号码**，
   投票也从"号码+窗口"改为按号码聚合（窗口取票数最多者）。
   否则窗口误读（`C04`/`CC04`/`C045`）每个都是新键，同一个号会被反复播报。

> ⚠️ 改 `ocr.pattern` 是**配置**改动，不随二进制走；部署新二进制时要一并改 `config.json`，
> 否则旧正则会让 `AT174` 重新出现（去重改按号码后只播一遍，但那一遍播的是错号）。

---

## 七、两个运维提醒

**功耗**：i3-7100 TDP 51W，24 小时开机整机约 30–45W，一年电费两三百。作为现场常驻设备可以接受，但心里要有这个账。

**Meltdown / Spectre**：Kaby Lake 属于受影响的世代，Linux 默认开启缓解措施，性能约有 5–15% 折扣。对这个负载完全无影响。

**磁盘写入**：`save_trigger_frames: true` 会持续往 `_live/` 写触发帧存证。生产稳定运行后可关闭以延长 SSD/存储寿命；排查期保留它，它是定位问题最有效的证据。
