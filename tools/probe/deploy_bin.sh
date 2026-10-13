#!/bin/bash
# 部署新版 announce-rs 二进制 + 重启配置台 (以 root 执行)
set -e
BIN=/opt/announce-rs/announce-rs
NEW=/home/debian13/build/target/release/announce-rs

echo "=== 1) 备份旧二进制 ==="
cp -a "$BIN" "$BIN.bak.$(date +%Y%m%d-%H%M%S)"
ls -1t /opt/announce-rs/announce-rs.bak.* 2>/dev/null | head -3

echo "=== 2) 停 announce-rs ==="
systemctl stop announce-rs
sleep 1
systemctl is-active announce-rs || true

echo "=== 3) 替换二进制 ==="
cp -f "$NEW" "$BIN"
chown debian13:debian13 "$BIN"
chmod 755 "$BIN"
md5sum "$NEW" "$BIN"
ls -l "$BIN"

echo "=== 4) 启动 announce-rs ==="
systemctl start announce-rs
sleep 5
echo -n "active: "; systemctl is-active announce-rs
systemctl show -p MainPID -p ExecMainStartTimestamp -p NRestarts announce-rs

echo "=== 5) 重启 announce-web (config_web.py 已更新) ==="
systemctl restart announce-web
sleep 2
echo -n "web active: "; systemctl is-active announce-web

echo "=== 6) announce-rs 启动日志 ==="
journalctl -u announce-rs -n 30 --no-pager
