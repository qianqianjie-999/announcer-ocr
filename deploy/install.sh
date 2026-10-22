#!/usr/bin/env bash
# install.sh — Linux 小主机一键部署（Debian/Ubuntu 系，树莓派/迷你主机通用）
# 用法: sudo bash install.sh
set -e

APP_DIR="$(cd "$(dirname "$0")" && pwd)"

echo "== 1/4 系统依赖 =="
apt-get update -qq
apt-get install -y -qq python3-venv python3-pip espeak-ng alsa-utils ffmpeg libgl1 || true

echo "== 2/4 Python 虚拟环境与依赖 =="
python3 -m venv "$APP_DIR/venv"
"$APP_DIR/venv/bin/pip" install --quiet -i https://pypi.tuna.tsinghua.edu.cn/simple \
    opencv-python-headless numpy rapidocr-onnxruntime pyttsx3

echo "== 3/4 音频与摄像头自检 =="
( speaker-test -t sine -f 440 -l 1 -p 1 >/dev/null 2>&1 && echo "音频输出: OK" ) || echo "音频输出: 失败(检查 aplay -l / 音频线 / audio 组权限)"
if ls /dev/video* >/dev/null 2>&1; then echo "USB摄像头: $(ls /dev/video* | tr '\n' ' ')"; else echo "USB摄像头: 未检测到(用RTSP可忽略)"; fi

echo "== 4/4 systemd 开机自启 =="
# ★2026-10-23 低危修复: root 直接登录执行时 $SUDO_USER 未设, User= 被替换成
#   空值 —— 部分 systemd 版本拒绝解析该 unit。回退为当前用户(root)。
RUN_USER="${SUDO_USER:-$(id -un)}"
sed "s|__APP_DIR__|$APP_DIR|g; s|__USER__|$RUN_USER|g" "$APP_DIR/announce.service" > /etc/systemd/system/announce.service
systemctl daemon-reload
systemctl enable announce
echo ""
echo "部署完成。后续命令:"
echo "  配置ROI(需桌面)后启动:  systemctl start announce"
echo "  查看日志:               journalctl -u announce -f"
