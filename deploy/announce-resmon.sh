#!/bin/bash
# announce-resmon.sh — 每 5 分钟记一行资源占用，用于回答"主码流长跑会不会涨内存/死机"
# 只读采样，不干预业务。字段: 时间, 系统已用/可用MB, ffmpeg RSS/CPU, 播报进程 RSS/CPU, load1, 温度
set -u
OUT=/var/log/announce-rs/resource.csv
[ -f "$OUT" ] || echo "ts,mem_used_mb,mem_avail_mb,ff_rss_kb,ff_cpu,rs_rss_kb,rs_cpu,load1,temp_c" > "$OUT"

MU=$(free -m | awk '/^Mem:/{print $3}')
MA=$(free -m | awk '/^Mem:/{print $7}')
FR=$(ps -eo rss,args | awk '/[f]fmpeg/{r+=$1} END{print r+0}')
FC=$(ps -eo pcpu,args | awk '/[f]fmpeg/{c+=$1} END{print c+0}')
RR=$(ps -eo rss,args | awk '/[a]nnounce-rs /{r+=$1} END{print r+0}')
RC=$(ps -eo pcpu,args | awk '/[a]nnounce-rs /{c+=$1} END{print c+0}')
L=$(cut -d' ' -f1 /proc/loadavg)
T=$(cat /sys/class/thermal/thermal_zone*/temp 2>/dev/null | sort -n | tail -1)
if [ -n "${T:-}" ]; then T=$((T / 1000)); else T=""; fi

echo "$(date '+%F %T'),${MU},${MA},${FR},${FC},${RR},${RC},${L},${T}" >> "$OUT"

# 5 分钟一条: 10000 行 ≈ 35 天，超出就先砍掉最老的
if [ "$(wc -l < "$OUT")" -gt 10000 ]; then
    tail -9000 "$OUT" > "$OUT.tmp" && mv "$OUT.tmp" "$OUT"
fi
