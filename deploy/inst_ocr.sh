#!/bin/bash
# inst_ocr.sh — 靶机安装 OCR/TTS/音频依赖（Debian 12）
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq tesseract-ocr tesseract-ocr-chi-sim espeak-ng alsa-utils
echo "=== 版本确认 ==="
tesseract --version 2>&1 | head -1
tesseract --list-langs 2>&1 | tail -3
espeak-ng --version
echo "=== 声卡 ==="
aplay -l 2>/dev/null | grep '^card' || echo "无声卡(检查HDMI音频)"
echo "=== 安装完成 ==="
