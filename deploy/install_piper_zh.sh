#!/bin/bash
# install_piper_zh.sh — 在 Debian 小主机上装 piper 神经语音 + 中文女声(华言 medium)
# 背景: espeak-ng 1.51 的中文音色名为 "Chinese (Mandarin, latin_as_English)",
#       音色老旧且把数字/字母按英语念, 现场听感很差 -> 改用 piper 神经网络 TTS。
# 依赖: 外网可访问 TUNA PyPI 镜像与 hf-mirror.com (GitHub 不通也能装)
# 用法: sudo bash install_piper_zh.sh [目标目录, 默认 /home/debian/anntest/tts]
set -e

DEST="${1:-/home/debian/anntest/tts}"
PIP_MIRROR="https://pypi.tuna.tsinghua.edu.cn/simple"
HF="https://hf-mirror.com/rhasspy/piper-voices/resolve/main/zh/zh_CN/huayan/medium"

echo "[1/4] 安装 python3-venv(如缺)"
apt-get install -y python3-venv >/dev/null 2>&1 || true

echo "[2/4] 建 venv 并安装 piper-tts"
mkdir -p "$DEST"
python3 -m venv "$DEST/env"
"$DEST/env/bin/pip" install -q --upgrade pip --index-url "$PIP_MIRROR"
"$DEST/env/bin/pip" install --index-url "$PIP_MIRROR" piper-tts

echo "[3/4] 下载中文女声模型(约 63MB)"
cd "$DEST"
curl -sL --max-time 900 -o zh_CN-huayan-medium.onnx        "$HF/zh_CN-huayan-medium.onnx"
curl -sL --max-time 120 -o zh_CN-huayan-medium.onnx.json   "$HF/zh_CN-huayan-medium.onnx.json"
ls -l zh_CN-huayan-medium.onnx*

echo "[4/4] 合成自测"
echo '请 G A 二 零 零 到 零 八 号 窗 口 办 理' \
  | "$DEST/env/bin/piper" --model "$DEST/zh_CN-huayan-medium.onnx" \
      --output_file /tmp/piper_selftest.wav
ls -l /tmp/piper_selftest.wav
aplay -D default /tmp/piper_selftest.wav || echo "(无声卡时忽略播放失败)"

cat <<EOF

完成。把下面三段写进 config.json 的 tts 段即可启用:
  "engine":       "piper",
  "piper_bin":    "$DEST/env/bin/piper",
  "piper_model":  "$DEST/zh_CN-huayan-medium.onnx",
  "piper_lib_dir": ""
(或在 Web 配置台点「测试播报」验证, 页面会显示实际使用的引擎)
EOF
