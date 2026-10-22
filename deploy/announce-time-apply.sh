#!/bin/bash
# announce-time-apply.sh — 消费配置台提交的手工校时请求（root 侧执行）
#
# 架构: 配置台(config_web.py)以 debian13 跑，改系统时间要 root。现场无 sudo，
# 用 su 则需在配置台里明文存 root 密码 —— 不可接受。改为"请求文件"：
#   配置台写  <dir>/request  (debian13 可写)
#   本脚本由 announce-time.timer (root) 每 5 秒取走、校验、执行，写 <dir>/result
# 参数只来自严格正则校验后的时间串，不经过 shell 求值，无注入面。
set -u
DIR=/var/lib/announce-rs/time
REQ="$DIR/request"
RES="$DIR/result"
MARK="$DIR/last_req_id"

[ -f "$REQ" ] || exit 0

# 原子取走（mv 后立刻改名，避免两个 timer 实例重复执行）
TMP="$DIR/req.$$"
mv -f "$REQ" "$TMP" 2>/dev/null || exit 0
RAW=$(head -c 200 "$TMP" 2>/dev/null | head -1 | tr -d '\r')
rm -f "$TMP"

# 请求格式: "<请求ID>|<YYYY-MM-DD HH:MM:SS>"，请求 ID 用于页面认领自己的结果
RID=${RAW%%|*}
T=${RAW#*|}
case "$RID" in ""|"$RAW") RID="";; esac

stamp() { date '+%F %T'; }

if [[ ! "$T" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}\ [0-9]{2}:[0-9]{2}:[0-9]{2}$ ]]; then
    # ★2026-10-23 低危修复: 原子写 —— 旧版直接 > "$RES", Web 端 5s 轮询可能读到半行
    echo "$(stamp) ERR ${RID}|时间格式必须是 YYYY-MM-DD HH:MM:SS（收到: ${T:0:40}）" > "$RES.tmp.$$"
    mv "$RES.tmp.$$" "$RES"
    chmod 644 "$RES" 2>/dev/null; exit 2
fi

Y=${T:0:4}; M=${T:5:2}; D=${T:8:2}; h=${T:11:2}; m=${T:14:2}; s=${T:17:2}
if (( Y < 2020 || Y > 2099 || 10#$M < 1 || 10#$M > 12 || 10#$D < 1 || 10#$D > 31 \
      || 10#$h > 23 || 10#$m > 59 || 10#$s > 59 )); then
    echo "$(stamp) ERR ${RID}|时间数值越界: $T" > "$RES.tmp.$$"
    mv "$RES.tmp.$$" "$RES"
    chmod 644 "$RES" 2>/dev/null; exit 3
fi

# 现场无外网，NTP 本就不工作；仍先关掉，免得将来接了网被立刻拽回旧时间
timedatectl set-ntp false >/dev/null 2>&1 || true

OLD=$(date '+%F %T')
if ! date -s "$T" >/dev/null 2>&1; then
    echo "$(stamp) ERR ${RID}|date -s 失败（缺少 CAP_SYS_TIME？）" > "$RES.tmp.$$"
    mv "$RES.tmp.$$" "$RES"
    chmod 644 "$RES" 2>/dev/null; exit 4
fi

# 写进 RTC：不写的话重启后又回到旧时间（这台机 RTC 走 UTC）
HW=$(command -v hwclock || echo /sbin/hwclock)
WARN=""
"$HW" -w >/dev/null 2>&1 || WARN="（RTC 写入失败，重启后可能回退）"

echo "$(stamp) OK ${RID}|$OLD -> $(date '+%F %T') $WARN" > "$RES.tmp.$$"
mv "$RES.tmp.$$" "$RES"
chmod 644 "$RES" 2>/dev/null
echo "$RID" > "$MARK" 2>/dev/null
exit 0
