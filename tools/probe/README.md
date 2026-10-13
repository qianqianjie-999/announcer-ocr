# tools/probe — 现场排查 / 调优脚本

2026-10-13 在一台真实叫号屏靶机（Debian 13 / i3-7100 / 4GB）上线时写的临时工具。
全部按**在该机上以 root 直接运行**设计，硬编码了 `/opt/announce-rs/...` 与 piper 模型路径；
换机器请先改路径。它们不改动仓库源码，只读写 `/opt/announce-rs/config.json`、调用
`announce-web` 的 `/api/test_tts`、或是对存证图跑一次离线 OCR 复现。

## 一、全链路模拟（不开摄像头也能验一遍）

| 脚本 | 作用 |
|---|---|
| `sim_announce.py <图>` | 用一张图走完整链路：tesseract（与主程序同参数）→ 复刻 Rust 的解析 → 拼播报文本 → piper 合成 → `aplay` 播 1 遍。用于"用图模拟播报"，不依赖现场叫号。 |
| `check_parse.py` | 只做「真实 tesseract → 复刻 `ocr.rs::parse()` 归一化」这一段，用于回归正则/号码清洗改动（`split_prefix`、`only_digits`、窗口去前导零）。 |

## 二、配置与参数核查

| 脚本 | 作用 |
|---|---|
| `interval.py` | 打印抓拍/识别节奏相关配置（`fps`、`capture_*`、`roi`、`detect.*`）与运行时实测值。 |
| `detect_hist.py` | 对比当前 `detect` 全字段与历次 `config.json.bak.*`，看参数是怎么被改动的。 |
| `speed_check.py` | 语速实测：页面是否含 `t_len` 输入框、piper 合成时长、最新一条播报。 |
| `check_settle.py` | 校验配置台 `validate_and_build()` 对 `detect.settle_sec` 的兜底钳位（空→0.5、≤0→0.1、超→3.0）。 |
| `verify_letter.py` | 校验 `tts.speak_window_letter` 开关（窗口里的字母 C 是逐字母念还是只念数字）。 |
| `verify_deploy.py` | 部署后校验：部署二进制 md5 与编译产物是否一致、页面是否可打开、试听文本是否正确。 |

## 三、TTS 诊断

| 脚本 | 作用 |
|---|---|
| `tts_diag.py` | 诊断 piper 中文模型对拉丁字母的发音：导出音素表 + 对比不同写法的合成能量。 |
| `probe_tts.py` | 统计调 `/api/test_tts` 时 `aplay` 进程出现的次数与时刻，用于确认"到底播了几遍"。 |

## 四、现场热修（会改 config.json，自动备份）

| 脚本 | 作用 |
|---|---|
| `fix_pattern.sh` | 修 OCR 正则，消除"请GA172到号窗口办理"这种半截播报（删掉"仅号码"候选、放宽窗口正则）。 |
| `fix_pattern2.py` | 把 `ocr.pattern` 锚定到 G 前缀（`G[A-Z]{0,2}\d{2,6}`）并补 `detect.settle_sec`，根治 `GAT174 → AT174` 的误切。 |
| `fix_detect.py` | 把 `detect` 的兜底/活跃期/确认窗口恢复成上游设计值（`fallback_ocr_sec` 等被网页写成 0 关掉后的修复）。 |
| `fix_cfg.sh` | 直接改 `tts.piper_length_scale`（示例 1.25）。 |
| `set_speed.sh [值]` | 改 piper 语速（默认 1.45，越大越慢）。 |
| `deploy_bin.sh` | 备份旧二进制 → 停服务 → 换 `announce-rs` → 重启 `announce-rs` 与 `announce-web` → 打印校验结果。 |

## 五、模拟图

`mock_screen.jpg`（白底黑字"请GA176到C04号窗口办理"）、`mock_ok.jpg`、`mock_extra.jpg`
（后者故意画成"GAT174"带多余字母）—— 靶机上没有中文字体也没有 PIL，这几张是在本地用
Pillow + `msyh.ttc` 画好后传上去的，用于喂 `sim_announce.py` / `check_parse.py`。

> 注：真实存证帧（`_live/trig_*.jpg`）与实际摄像头抓图**未入库**，避免把现场画面公开。
