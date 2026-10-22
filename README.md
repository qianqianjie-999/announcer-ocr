# announce-rs — 叫号屏摄像头 OCR 语音播报

用摄像头拍摄叫号屏（或测试卡），自动识别"号码+窗口"并用中文语音播报的无人值守服务。
Rust 实现，纯 Rust 依赖（零 C 绑定，可静态交叉编译），单二进制 + 一个 config.json 即可部署。

## 功能特性

- 四种画面源统一接入：USB 摄像头 / `/dev/videoN` / RTSP 网络流 / 本地图片模拟
- **Web 配置调试台**：浏览器里拖框标定 ROI、试听 TTS、自测 OCR、管理服务（零 pip 依赖）
- 帧差变化检测触发 OCR，屏面不动不烧 CPU
- tesseract 中文识别 + 形近字/误插零噪声清洗，双正则提取"号码→窗口"
- 非阻塞确认状态机：候选多轮投票 + 静帧快速确认，端到端播报延迟 1~3s
- 双引擎 TTS：espeak-ng（轻量兜底）/ piper（中文真人音色），支持常驻合成服务
- 播报去重 TTL、CSV 日志留痕（utf-8-sig，Excel 直开）、触发帧存证
- ffmpeg 断流自动重启、单实例锁、日志写失败不影响主流程

## 系统架构

```
                    ┌─────────────────────────────────────────────┐
                    │                main.rs 主循环 (fps=3)        │
                    │                                             │
 ┌──────────┐  帧   │  ┌──────────┐   ┌──────────┐   ┌─────────┐  │
 │ capture  │──────▶│  │  detect  │──▶│   ocr    │──▶│announcer│  │
 │ 帧采集    │ 最新帧 │  │ 变化检测  │触发│ 识别解析  │候选│ 播报状态机│  │
 └──────────┘       │  └──────────┘   └──────────┘   └────┬────┘  │
  ffmpeg 管道        │   帧差+亮度对齐    tesseract+正则      │确认通过 │
  v4l2/RTSP/模拟     │                                    ▼       │
                    │                              ┌──────────┐  │
                    │                              │   tts    │  │
                    │                              │ 语音合成  │  │
                    │                              └────┬─────┘  │
                    │                                   ▼        │
                    │                            aplay / 音频设备  │
                    └─────────────────────────────────────────────┘
                                    │ 每次播报
                                    ▼
                          logger.rs → announce_log.csv
```

## 快速开始

```bash
# 依赖: rust(cargo) / ffmpeg / tesseract(+chi_sim 语言包) / aplay 或 pipewire
cargo build --release

# 1. 模拟模式(无需摄像头, 读 sim_frame.jpg)
./target/release/announce-rs config.json

# 2. 摄像头模式
./target/release/announce-rs config.json        # camera_source: "/dev/video0"

# 运行单元测试
cargo test --release
```

最小配置（其余项全部有默认值，见 [docs/06-config.md](docs/06-config.md)）：

```json
{
  "camera_source": "/dev/video0",
  "roi": [640, 330, 880, 520],
  "ocr": { "pattern": ["^([A-Z])(\\d{3})$"] }
}
```

> ★ 2026-10-23 更正：`ocr.pattern` 与 `roi` 都是启动硬性必配——缺 pattern 启动即
> `exit(1)`（src/ocr.rs），缺 roi 直接退出（src/main.rs），不存在所谓"默认 pattern"。

> ROI 强烈建议按现场标定收紧到叫号屏区域：背景杂物会严重污染识别（实测教训）。

## 停机、重启与长稳维护

2026-10-11 起，这三件事由程序自己负责，不再依赖部署层的定时器兜底：

| 场景 | 行为 |
|---|---|
| `systemctl restart` / `Ctrl+C` / `kill <pid>` | 收到 SIGTERM/SIGINT → 播完队列 → 收掉 ffmpeg 子进程 → 退出码 0（**不留孤儿进程**） |
| 号码跨天复用 | 去重表每天本地零点自动清理（`announced_ttl_sec: 0`）；也可配滚动 TTL |
| 日志写满 | 超过 `log_max_mb`（默认 8MB）自动归档到 `logs_archive/`，只留最近 `log_keep`（默认 6）个 |

> **升级提示**：`announced` 的语义从"永不重复"变为"当天不重复"。若现场确实需要
> "一个号码一整天只播一次、跨天也不重播"，把 `announced_ttl_sec` 设为负数即可回到旧行为。

journal 里的维护留痕（用于确认治理在工作）：`[退出] 收到停止信号…`、`[退出] 已优雅停机`、
`[维护] 去重表清理: …`、`[日志] 已达阈值, 轮转 -> …`、`[启动] 清理 _live/ 上次遗留的临时文件 N 个`。

## 目录结构

```
anntest-src/
├── src/
│   ├── main.rs        # 程序入口 + 主循环编排（确认状态机在这里）
│   ├── capture.rs     # 帧采集：ffmpeg 管道 / 模拟模式，断流自愈
│   ├── detect.rs      # 变化检测：灰度缩放 + 亮度对齐帧差 + OCR 增强预处理
│   ├── ocr.rs         # tesseract 子进程调用 + 噪声清洗 + 正则解析
│   ├── announcer.rs   # 播报器：语音队列 + 优雅退出前排空
│   ├── tts.rs         # 语音合成：espeak / piper 双引擎
│   ├── logger.rs      # CSV 播报日志 + 按大小轮转归档
│   ├── dedupe.rs      # 去重表的时间治理（每日零点 / TTL / 硬上限）
│   ├── signal.rs      # SIGTERM/SIGINT 优雅退出（置停止标志）
│   ├── cfg.rs         # 配置结构（serde 反序列化，全字段有默认值）
│   └── （配套）config_web.py  # Web 配置调试台（Python 标准库，端口 8080）
├── docs/              # 按功能分块的详细文档（见下）
├── deploy/            # 靶机部署资产：systemd 单元 + 安装/清理脚本（见「部署」节）
└── config.json        # 运行配置
```

## 文档索引（按功能分块）

| 文档 | 内容 |
|---|---|
| [01-capture.md](docs/01-capture.md) | 帧采集：画面源抽象、管道排空、ffmpeg 自愈重启 |
| [02-detect.md](docs/02-detect.md) | 变化检测：预处理流水线、亮度对齐帧差、OCR 图像增强 |
| [03-ocr.md](docs/03-ocr.md) | 识别解析：子进程调用、噪声清洗规则、正则与归一化 |
| [04-announcer.md](docs/04-announcer.md) | 播报链路：确认状态机、去重 TTL、CSV 日志 |
| [05-tts.md](docs/05-tts.md) | 语音合成：双引擎对比、号码逐字读法、无 root 安装 |
| [06-config.md](docs/06-config.md) | 配置参考：全部字段、默认值、调优建议 |
| [07-troubleshooting.md](docs/07-troubleshooting.md) | 实战排障：踩坑记录与现场处置 |
| [08-webui.md](docs/08-webui.md) | Web 配置调试台：ROI 拖框、OCR/TTS 自测、API 与安全设计 |
| [09-rtsp-field-checklist.md](docs/09-rtsp-field-checklist.md) | 现场 RTSP 摄像头接入操作清单（打印携带用） |
| [10-quality-review.md](docs/10-quality-review.md) | 生产级稳定性评审：量化指标、问题分级、优化实施记录与实测证据 |

## 部署（deploy/）

靶机（Debian，部署根 `/home/debian/anntest`）的**运维即代码**，与主程序同仓库、同版本管理。
⚠️ 这些文件描述的是**靶机布局**，不参与本地编译。

Rust 版当前在用：

| 文件 | 作用 |
|---|---|
| `announce-rs.service` | 主服务：`Restart=always`、`RestartSec=5`、`CPUQuota=300%`、`MemoryMax=300M` |
| `announce-web.service` | Web 配置调试台（8080，**仅供内网调试**，故未加鉴权） |
| `announce-piper.service` | 常驻 piper TTS 服务（避免每句重载模型） |
| `announce-worktime-{start,stop}.{service,timer}` | 工作时段自动启停：每日 08:00 起、18:00 停（ExecStart 指向 `announce-worktime-act.sh`，幂等护栏：深夜补跑 start 不会拉起服务） |
| `announce-worktime-act.sh` | worktime 幂等执行体（读 config.json 的 schedule 决定执行/跳过） |
| `announce-time.{service,timer}` + `announce-time-apply.sh` | Web 台手工校时：timer 每 5s 消费校时请求 |
| `sudoers.announce-web` | Web 台 sudo 窄授权片段（restart / net-apply / schedule-apply 三条） |
| `announce-resmon.sh` | 资源巡检（CPU/内存/磁盘，写 CSV） |
| `audio_health.sh` | 播报音频链路自检/自愈（声卡复位→USB 热复位→重启升级链） |
| `announce-cleanup.{service,timer}` + `announce-cleanup.sh` | 每日 03:30 清理：CSV 轮转、存证按天保留、journald 限额 |
| `tts_server.py`、`install_piper_zh.sh` | piper 服务端与中文模型安装 |
| `install_cleanup.sh`、`inst_ocr.sh` | 清理与 OCR 环境安装脚本 |
| `announce-net-apply.sh` | 改 IP（**应用后 90 秒内需确认，否则自动回滚**） |
| `check_host.py`、`config.example.py` | 环境自检 / 配置模板 |

历史遗留（Python 版，Rust 版不用）：`announce.service`、`announce-user.service`、`install.sh`。
详细步骤见各 `install_*.sh` 脚本内注释。

> ⚠️ `deploy/config_web.py` 是 2026-09 的历史快照，与仓库根目录的 `config_web.py` 已不一致。
> **配置台一律以根目录的 `config_web.py` 为准。**
