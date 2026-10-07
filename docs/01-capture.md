# 01 · 帧采集（capture.rs）

## 职责

把四种画面源抽象成统一的 `Frame { w, h, rgb }` 最新帧接口，供主循环以固定 fps 消费。

```
camera_source 配置          SourceKind          实现
─────────────────────────────────────────────────────────
0 / "/dev/video0"    →   Device(path)   →   ffmpeg 管道
"rtsp://..." URL     →   Net(url)       →   ffmpeg 管道
"simulate"           →   Simulate       →   每次重读 sim_frame.jpg
```

类型分发在 [cfg.rs `source()`](../src/cfg.rs#L112-L134)，取帧实现是 `Source::read()`。

## 为什么走 ffmpeg 子进程

1. **零 C 绑定**：不依赖 opencv/gstreamer，纯 Rust 依赖树才能静态交叉编译到弱性能靶机
2. **统一解码**：ffmpeg 自带 v4l2/RTSP 解码与低延迟参数，与 Python 版
   `OPENCV_FFMPEG_CAPTURE_OPTIONS` 行为同源
3. 捕获参数（分辨率/低延迟 flags）在 [main.rs](../src/main.rs) 组装后传入 `PipeGrabber::start()`

## 管道排空线程（关键设计）

主循环只有 fps=3，若不排空，解码缓冲会持续积压、延迟无限上涨。

```
ffmpeg stdout ──满帧率读取──▶ 后台线程 ──只保留最新──▶ Arc<Mutex<Option<Frame>>>
                                                    主循环永远取这一帧
```

- 后台线程以 1MB 缓冲 `read_exact` 读 rawvideo(bgr24)，转 RGB 后覆盖写 `latest`
- 主循环 `Source::read()` 只 clone `latest` 指针，永不阻塞在解码上

## ffmpeg 断流自愈（无人值守核心）

首次启动失败仍快速报错（配置错误要立刻暴露）；**运行中断流则无限重试**：

```
管道读取结束/子进程退出
  → kill + wait 收尸（否则变 defunct 僵尸，实测会一直挂在 ps 里）
  → sleep 3s → 重新 spawn ffmpeg（失败则 5s 后再试）
  → 直到 Drop（程序退出）为止
```

背景：首次启动时 ffmpeg 若因设备被上一个实例的孤儿进程短暂占用而秒退，
旧实现会永久卡死在"取帧失败"，只能人工重启。自愈逻辑见
[capture.rs `PipeGrabber::start()`](../src/capture.rs#L45)。

## 模拟模式

`Source::Simulate` 每次调用都重新读文件并解码——测试台脚本可以**原子替换**
`sim_frame.jpg` 来模拟换屏，无需真实摄像头即可跑完整链路（自测/回归用）。

## RTSP 源要点（2026-10-07 端到端实测）

`camera_source: "rtsp://user:pass@ip:554/stream1"` 即生效，实测链路
`mediamtx(H.264) → ffmpeg TCP 拉流 → 帧差/OCR 全通`：

- **强制 TCP 传输**（`-rtsp_transport tcp`）：UDP 花屏帧会污染 OCR
- 低延迟三件套：`nobuffer + low_delay + max_delay 300ms`
- **RTSP 帧尺寸可能变**（重连/降流），ROI 出界防御已加入——`prepare()`
  返回空图、主循环跳帧并告警一次；此前该路径会数组越界 panic（USB 恒定
  分辨率从未暴露）
- 现场建议用**子码流**（640×360 即够叫号屏识别），省带宽且解码更轻

## 相关配置

| 字段 | 默认 | 说明 |
|---|---|---|
| `camera_source` | `"/dev/video0"` | 数字索引 / 设备路径 / rtsp URL / "simulate" |
| `capture_width/height` | 1920×1080 | 请求 ffmpeg 输出分辨率 |
| `fps` | 3 | 主循环取帧频率（非解码帧率） |
