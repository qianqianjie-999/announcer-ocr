# 05 · 语音合成（tts.rs）

## 职责

把播报文案变成声音。双引擎自动选择，独占线程消费播报队列，逐条播放。

## 引擎对比与选择

| | espeak-ng | piper |
|---|---|---|
| 音质 | 机器音 | 中文真人音色（zh_CN-huayan） |
| 合成耗时 | 快 | CLI 模式弱靶机约 13s/条；本机 1.57s/条 |
| 拉丁字母 | **设计上按英语读**（VoiceName latin_as_English）——中文场景必配 `spell_number` | 可直接读 |
| 依赖 | 系统包 | onnxruntime + Python venv + 模型(~60MB) |

`engine: "auto"` 的选择逻辑：配置了 piper 且环境可用 → piper；否则回退 espeak。
选型结论：**生产用 piper，espeak 仅作兜底**。

## piper 三种运行方式（按配置自动降级）

```
piper_url 配置了?  ──是──▶ HTTP 常驻服务(约2~3s出声, 服务挂了自动回退 CLI)
        │否
        ▼
    CLI 直调(piper --model xxx → wav 文件 → aplay)
```

CLI 直调每次都要重载 onnx 模型，弱靶机上 13s 不可接受——生产应部署常驻服务：

```bash
# 常驻合成服务(示例): 收 POST 文本, 回 wav
python3 -m piper.http_server -m zh_CN-huayan-medium.onnx --port 5001
# config.json: "piper_url": "http://127.0.0.1:5001/"
```

## 号码逐字读法（spell_number）

中文播报场景 `GA200` 不能读成英语单词、`C08` 不能读成"C零八"以外的东西。
`spell_number: true` 时先做 token 化再合成：

```
请GA200到C08号窗口办理 → 请 G A 二 零 零 到 C 零 八 号窗口办理
```

字母逐个读，数字映射为中文（`0→零 1→一 ... 9→九`）。

## 语速控制

| 引擎 | 参数 | 说明 |
|---|---|---|
| espeak | `rate`（默认 135） | 实际 wpm = max(80, rate×0.7) ≈ 94，偏慢是刻意的 |
| piper | `piper_length_scale`（默认 1.0） | **越大越慢**，稍慢推荐 1.2；此前 rate 对 piper 无效，2026-10-07 补的透传 |

## 播放链路

```
合成 wav(/tmp 下唯一文件名) → aplay[-D 设备] → 播完删除 → repeat 遍
```

- `repeat: 2`：每条播 2 遍（现场嘈杂，单遍易漏听）
- `gap_sec: 0.35`：两遍之间的间隔
- `aplay_device`：多声卡时指定，如 `"plughw:1,0"`

## 无 root 安装（现场应急）

靶机无 sudo 时不用源码编译，`apt download` 拉.deb 解包到用户目录：

```bash
apt download espeak-ng espeak-ng-data libespeak-ng1 libsonic0 ...
mkdir -p ~/tts-root && for f in *.deb; do dpkg -x $f ~/tts-root/; done
# 进程环境注入(启动脚本):
export PATH="$HOME/tts-root/usr/bin:$PATH"
export LD_LIBRARY_PATH="$HOME/tts-root/usr/lib/x86_64-linux-gnu:$LD_LIBRARY_PATH"
export ESpeakNgDataPath="$HOME/tts-root/usr/lib/espeak-ng-data"
```

piper venv 同理可用目标机 python 重建（注意版本：py3.11 建的 venv 在 3.12 上不可用，
需 `python3 -m venv` 重造后复用模型文件）。
