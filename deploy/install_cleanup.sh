#!/bin/bash
# 安装日志自动清理: journald 硬上限(drop-in) + 每日清理 timer。以 root 执行。
# 幂等: 可重复执行, 不会重复添加配置。
set -eu

DIR=/home/debian/anntest
PKG="$DIR/_pkg"
say() { echo "[install] $*"; }

[ -d "$PKG" ] || { echo "缺 $PKG (安装包目录)"; exit 1; }

# —— 1. journald 上限: 用 drop-in, 不改主配置, 删文件即可回滚 ——
mkdir -p /etc/systemd/journald.conf.d
cat > /etc/systemd/journald.conf.d/99-announce.conf <<'EOF'
# 由叫号播报项目安装(announce-cleanup.timer 配套)。
# 小主机 7×24 运行, 主程序与配置台都会往 journal 写日志,
# 给一个硬上限防止日志把 eMMC/磁盘写满; 超出时 journald 自动丢弃最旧的。
[Journal]
SystemMaxUse=64M
SystemMaxFileSize=16M
SystemMaxFiles=5
MaxRetentionSec=1month
EOF
say "journald 上限 -> /etc/systemd/journald.conf.d/99-announce.conf"

# —— 2. 清理脚本 ——
install -m 0755 "$PKG/announce-cleanup.sh" "$DIR/announce-cleanup.sh"
say "清理脚本 -> $DIR/announce-cleanup.sh"

# —— 3. systemd 单元 ——
install -m 0644 "$PKG/announce-cleanup.service" /etc/systemd/system/announce-cleanup.service
install -m 0644 "$PKG/announce-cleanup.timer"   /etc/systemd/system/announce-cleanup.timer
systemctl daemon-reload
say "已装入 announce-cleanup.service + .timer"

# —— 4. 让 journald 上限立即生效 ——
systemctl restart systemd-journald
say "journald 已重启"

# —— 5. 启用每日清理 ——
systemctl enable --now announce-cleanup.timer >/dev/null 2>&1
say "announce-cleanup.timer 已启用"

echo "---- 定时器状态 ----"
systemctl list-timers announce-cleanup.timer --no-pager | head -3
echo "---- 合并后的 journald 配置(关键项) ----"
systemd-analyze cat-config systemd/journald.conf 2>/dev/null | grep -E 'SystemMax|MaxRetention' | sed 's/^/  /'
echo "[install] 完成"
