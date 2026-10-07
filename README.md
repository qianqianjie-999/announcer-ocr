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
  "roi": [640, 330, 880, 520]
}
```

> ROI 强烈建议按现场标定收紧到叫号屏区域：背景杂物会严重污染识别（实测教训）。

## 目录结构

```
anntest-src/
├── src/
│   ├── main.rs        # 程序入口 + 主循环编排（确认状态机在这里）
│   ├── capture.rs     # 帧采集：ffmpeg 管道 / 模拟模式，断流自愈
│   ├── detect.rs      # 变化检测：灰度缩放 + 亮度对齐帧差 + OCR 增强预处理
│   ├── ocr.rs         # tesseract 子进程调用 + 噪声清洗 + 正则解析
│   ├── announcer.rs   # 播报器：去重 TTL + 语音队列
│   ├── tts.rs         # 语音合成：espeak / piper 双引擎
│   ├── logger.rs      # CSV 播报日志
│   ├── cfg.rs         # 配置结构（serde 反序列化，全字段有默认值）
│   └── （配套）config_web.py  # Web 配置调试台（Python 标准库，端口 8080）
├── docs/              # 按功能分块的详细文档（见下）
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
```
