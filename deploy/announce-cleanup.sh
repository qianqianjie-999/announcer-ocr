#!/bin/bash
# 叫号播报系统 —— 每日日志清理(由 announce-cleanup.timer 以 root 调用)。
# 目的: 小主机 7×24 运行, 日志/存证只增不减迟早写满磁盘、也白耗 eMMC 写入寿命。
#
# 安全约束(重要):
#   * 所有读写都限定在 $DIR(/home/debian/anntest) 及其子目录内;
#   * 只用"明确文件名模式"(announce_log_*.csv / trig_*.jpg / *.bak.*), 绝不 rm -rf、绝不通配符横扫;
#   * 归档只轮转不删除(保留最近 $ARCHIVE_KEEP 个), 仅磁盘告急时才激进清理;
#   * 输出直接进 journal, 由 journald 的 SystemMaxUse 统一封顶, 不再自己维护一个日志文件。
set -u

DIR=/home/debian/anntest
LIVE="$DIR/_live"
ARCHIVE="$DIR/logs_archive"
CSV="$DIR/announce_log.csv"

CSV_MAX=$((8 * 1024 * 1024))   # 播报日志超 8MB 轮转
ARCHIVE_KEEP=6                 # 保留最近 6 个归档
TRIG_KEEP_DAYS=7               # 存证截图保留 7 天
BAK_KEEP_DAYS=30               # *.bak.* 部署备份保留 30 天
TIGHT_KB=512000                # 可用空间低于 500MB 触发激进清理

say() { echo "[cleanup $(date '+%F %T')] $*"; }

say "=== 开始 (DIR=$DIR) ==="
mkdir -p "$ARCHIVE" 2>/dev/null
[ -d "$LIVE" ] || mkdir -p "$LIVE" 2>/dev/null

# 1) 播报日志轮转 —— 只在超阈值时动, 历史归档不丢
if [ -f "$CSV" ]; then
  SZ=$(stat -c%s "$CSV" 2>/dev/null || echo 0)
  if [ "$SZ" -gt "$CSV_MAX" ]; then
    DST="$ARCHIVE/announce_log_$(date +%Y%m%d_%H%M%S).csv"
    if mv -f "$CSV" "$DST" 2>/dev/null; then
      : > "$CSV"   # 留 0 字节文件: 主程序检测到空文件会补写 BOM, Excel 打开不乱码
      say "CSV 轮转: $SZ 字节 -> $(basename "$DST")"
    fi
  else
    say "CSV $SZ 字节, 未达阈值 $CSV_MAX, 跳过"
  fi
else
  say "CSV 不存在, 跳过"
fi

# 2) 归档只留最近 N 个(按 mtime 排序, 名称即时间序)
if [ -d "$ARCHIVE" ]; then
  N=$(ls -1 "$ARCHIVE"/announce_log_*.csv 2>/dev/null | wc -l)
  if [ "$N" -gt "$ARCHIVE_KEEP" ]; then
    ls -1t "$ARCHIVE"/announce_log_*.csv 2>/dev/null | tail -n +$((ARCHIVE_KEEP + 1)) |
    while IFS= read -r f; do
      # 双重保险: 只删确实匹配归档模式的普通文件
      if [ -f "$f" ] && [ "${f#/home/debian/anntest/logs_archive/announce_log_}" != "$f" ]; then
        rm -f -- "$f" && say "删除旧归档: $(basename "$f")"
      fi
    done
  else
    say "归档 $N 个, 未超 $ARCHIVE_KEEP, 跳过"
  fi
fi

# 3) 存证截图: 主程序自身上限 20 张(文件名固定会覆盖), 这里按时间兜底(超 7 天)
find "$LIVE" -maxdepth 1 -type f -name 'trig_*.jpg' -mtime +"$TRIG_KEEP_DAYS" -delete 2>/dev/null
say "存证截图现存 $(ls -1 "$LIVE"/trig_*.jpg 2>/dev/null | wc -l) 张(已删 ${TRIG_KEEP_DAYS} 天前的)"

# 4) 预览临时残留: 原子写中途中断会留下 .tmp; 陈旧请求文件(>1 小时没人消费)也清掉
find "$LIVE" -maxdepth 1 -type f -name 'latest.jpg.tmp' -mtime +1 -delete 2>/dev/null
find "$LIVE" -maxdepth 1 -type f -name 'req' -mmin +60 -delete 2>/dev/null

# 5) 部署备份超期清理(每次部署会留 announce-rs.bak.* / config_web.py.bak.*)
find "$DIR" -maxdepth 1 -type f -name '*.bak.*' -mtime +"$BAK_KEEP_DAYS" -delete 2>/dev/null

# 6) journald 兜底收缩(正常情况下 SystemMaxUse=64M 已自动管理, 这条是保险)
journalctl --vacuum-size=64M >/dev/null 2>&1 && say "journal 已收缩到 64MB 以内"

# 7) 磁盘告急时激进清理(仅限本目录的白名单文件)
AVAIL_KB=$(df -k "$DIR" 2>/dev/null | awk 'NR==2{print $4}')
if [ -n "${AVAIL_KB:-}" ] && [ "$AVAIL_KB" -lt "$TIGHT_KB" ]; then
  say "⚠ 可用空间仅 $((AVAIL_KB / 1024))MB (<500MB), 执行激进清理"
  find "$LIVE" -maxdepth 1 -type f -name 'trig_*.jpg' -delete 2>/dev/null
  find "$ARCHIVE" -maxdepth 1 -type f -name 'announce_log_*.csv' -delete 2>/dev/null
  find "$DIR" -maxdepth 1 -type f -name '*.bak.*' -delete 2>/dev/null
  journalctl --vacuum-size=32M >/dev/null 2>&1
  say "激进清理完成, 现在可用 $(( $(df -k "$DIR" | awk 'NR==2{print $4}') / 1024 ))MB"
fi

# 8) 汇总
say "磁盘: $(df -h "$DIR" 2>/dev/null | awk 'NR==2{print $3" / "$2" ("$5"), 可用 "$4}')"
say "journal: $(journalctl --disk-usage 2>/dev/null | sed 's/.*take up //')"
say "announce_log.csv: $(stat -c%s "$CSV" 2>/dev/null || echo 0) 字节"
say "announce-rs 内存: $(systemctl show announce-rs -p MemoryCurrent --value 2>/dev/null | awk '{printf "%.1f MB", $1/1048576}')"
say "=== 完成 ==="
