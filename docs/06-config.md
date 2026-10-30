# 06 · 配置参考（cfg.rs）

全字段均有默认值。**启动硬性必配：`camera_source`、`roi`、`ocr.pattern`**——缺 pattern
启动即 `exit(1)`，缺 roi 直接退出；不存在"默认 pattern"。schema 与 Python 版完全一致。

## 顶层

| 字段 | 类型 | 默认 | 说明 |
|---|---|---|---|
| `camera_source` | 数字/字符串 | `"/dev/video0"` | `0`/`"/dev/videoN"`/`"rtsp://..."`/`"simulate"` |
| `roi` | [x,y,w,h] | null(全画幅) | **必配**。只在此区域做检测+识别，背景杂物严重污染识别 |
| `fps` | f64 | 3.0 | 主循环频率 |
| `capture_width/height` | u32 | 1920/1080 | 请求 ffmpeg 输出分辨率 |
| `frame_stale_sec` | f64 | 60 | 无帧看门狗：连续这么久没新帧就 kill ffmpeg 重拉（**0 = 关闭**）。治 RTSP 半开 |
| `lock_port` | u16 | 51888 | 单实例锁（回环 TCP 占用，重复启动立即退出） |
| `log_file` | str | `announce_log.csv` | 播报日志路径 |
| `announced_ttl_sec` | f64 | 0 | 已播报去重表的过期策略（秒）：**0 = 每日本地零点清零**（默认，与"号码按天复用"业务语义一致）；`>0` = 滚动 TTL；`<0` = 永不清理（旧行为，仍有 4096 条硬上限兜底） |
| `log_max_mb` | f64 | 8.0 | 播报日志按大小轮转阈值（MB）。`0` = 关闭轮转（把治理完全交给 `deploy/announce-cleanup.sh`） |
| `log_keep` | u32 | 6 | 日志归档保留个数（不含当前文件），与清理脚本的 `ARCHIVE_KEEP` 对齐 |
| `log_archive_dir` | str | `logs_archive` | 日志归档目录，仅在 `log_max_mb > 0` 时使用 |
| `save_trigger_frames` | bool | true | 触发帧存证 `_live/`（生产稳定后可关，省 SD 写） |
| `diag_heartbeat` | bool | true | 心跳诊断日志 |
| `diag_every_sec` | f64 | 5.0 | 心跳间隔 |

## detect（变化检测与确认）

| 字段 | 默认 | 说明 |
|---|---|---|
| `diff_threshold` | 25.0 | 帧差单像素灰度阈（0-255） |
| `changed_ratio` | 0.0015 | 超阈占比≥此值 → 判定变化。误触发多→调大；换屏不触发→调小 |
| `stable_frames` | 2 | 候选复现确认轮数 |
| `confirm_ttl_sec` | 4.0 | 确认窗口，下限自动抬到 3×OCR 耗时，并覆盖空转退避间隔（见下） |
| `active_sec` | 3.0 | 变化后活跃期长度 |
| `min_interval_sec` | 0.5 | 两次 OCR 最小间隔 |
| `fallback_ocr_sec` | 3.0 | 常规兜底 OCR 间隔 |
| `static_fb_sec` | 10.0 | 静帧兜底间隔（静止时放宽省 CPU） |
| `static_confirm_ticks` | 2 | 静帧快速确认轮数，0=退回两轮投票 |
| `settle_sec` | 0.5 | 稳帧等待：画面停稳这么久才抓帧识别，避免拿过渡帧去 OCR |
| `max_jump` | 2 | 号码闸门的第一档边界：跳幅 ≤3 立即播，≤此值多 2 个确认，更大到 +30 多 4 个，>30 直接挡（0=关闭闸门） |
| `number_pool_max` | 400 | 号码数字段上限。超过此值直接判 `INVALID`，不进入确认和静默兜底；允许 1~9999 |
| `rows` | 1 | 识别行数。屏幕是多行列表时设 3，配合 `row_pitch` 逐行裁切识别 |
| `row_pitch` | 0 | 行距（**缩放后画布的绝对像素**）。0 或 `rows=1` 时退化为单行。**换码流/换分辨率必须重新标定** |
| `row_pitch_ratio` | 0 | 行距 = roi 高度 × 本值。**推荐用这个**（实测行距≈行高，常用 `1.0`），换分辨率自动跟随；仅在 `row_pitch=0` 时生效 |
| `highlight_row` | 自动 | 固定高亮行（1-based）；省略或设为 0 时自动比较各行红色通道中位数。现场反光或版式变化导致自动判错时，可设为 `2` 等明确行号 |

> 两个字段都给时以 `row_pitch` 为准。启动时若 `row_pitch` 超过行高的 1.6 倍会打印
> `[警告] row_pitch=... 疑似未按当前分辨率重新标定` —— 这个护栏就是为"换码流沿用旧值"准备的
> （2026-10-22 现场：辅码流画布下的 73 被按比例放大成 137，而真实行距只有 70）。

## ocr（识别）

| 字段 | 默认 | 说明 |
|---|---|---|
| `bin` | `"tesseract"` | 可执行文件路径 |
| `lang` | `"chi_sim"` | 语言包（本地无包时 `TESSDATA_PREFIX` 指向自备 tessdata） |
| `psm` | 11 | 稀疏文本模式，适配卡片布局 |
| `dpi` | 150 | |
| `pattern` | 双正则 | 单条字符串或数组；号码+窗口提取的核心，改前先跑 `cargo test` |

推荐 pattern（**非默认**——pattern 无默认值，必须显式配置，缺了启动即 exit(1)）：

```json
["([A-Z]{1,2}\\d{3,6})",
 "([A-Z]{1,2}\\d{3,6})[到至]([A-Z]{0,2}\\d{1,3})[号窗]"]
```

## tts（语音）

| 字段 | 默认 | 说明 |
|---|---|---|
| `enabled` | true | false 则只打 `[播报]` 日志不出声（自测用） |
| `engine` | `"auto"` | `"auto"/"espeak"/"piper"` |
| `template` | `请{number}到{window}号窗口办理` | `{number}/{window}` 占位 |
| `spell_number` | false | true 时号码逐字中文读：GA200→"G A 二 零 零" |
| `repeat` | 2 | 播报遍数 |
| `rate` | 135 | espeak 语速（对 piper 无效） |
| `piper_length_scale` | 1.0 | piper 语速缩放，越大越慢，稍慢 1.2 |
| `gap_sec` | 0.35 | 遍间隔 |
| `aplay_device` | null | 多声卡时指定，如 `"plughw:1,0"` |
| `piper_bin/model` | null | piper 可执行与 onnx 模型路径 |
| `piper_lib_dir` | null | onnxruntime 库目录（LD_LIBRARY_PATH 注入） |
| `piper_url` | null | 常驻合成服务地址，命中则 2~3s 出声，挂了自动回退 CLI |

## 调优速查

| 现象 | 动作 |
|---|---|
| 换屏不触发 | `changed_ratio` ↓（0.0015→0.001）；确认 roi 是否盖住屏幕 |
| 空场景反复 OCR | `changed_ratio` ↑；检查自动曝光（亮度对齐已兜底大部分） |
| 播报延迟高 | 确认 `static_confirm_ticks`≥2；OCR 慢查 `OMP_THREAD_LIMIT=1` 与 ROI 大小 |
| 同号重复播报 | 检查是否形近误读产生新键（C01/C001 类），先补测试用例再改 ocr.rs |
| 号读成英语 | piper 场景 `spell_number: true`；espeak 场景必须开 |
