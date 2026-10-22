#!/bin/bash
# announce-worktime-act.sh start|stop —— worktime timer 的幂等执行体(2026-10-22)
#
# 背景: start/stop 双 timer 都 Persistent=true, 主机停机跨过 08:00 与 18:00
# 后开机, systemd 会把两个错失事件都补跑且顺序不定 —— 深夜开机可能
# "先 stop 后 start"把服务整夜拉起(播报吵人/整夜跑)。
# 让执行体按当前时间自证合法性, 补跑顺序颠倒也无害:
#   start: 只有当前在 work_start~work_end 窗口内才执行(08:00 触发时在窗口;
#          23:00 补跑 08:00 事件 -> 不在窗口 -> 跳过, 防整夜跑)
#   stop : 只有当前不在窗口内才执行(18:00 触发时已过 end -> 执行;
#          白天补跑 stop 事件 -> 仍在窗口 -> 跳过, 防白天被误停)
# 作息参数读 /opt/announce-rs/config.json 的 schedule 键; 读不到/损坏时
# 保守放行原动作(退回旧版行为, 由主程序自身的作息闸兜底)。
set -u
ACT=${1:?用法: announce-worktime-act.sh start|stop}
CFG=${ANNOUNCE_CFG:-/opt/announce-rs/config.json}

SVC="announce-piper.service announce-rs.service announce-web.service"

# 从 config.json 读 schedule(纯 python3, 现场必有); 失败 => 空串表示读不到
read_cfg() {
    python3 - "$CFG" 2>/dev/null <<'PY'
import json, sys
try:
    c = (json.load(open(sys.argv[1])).get("schedule") or {})
    ws = str(c.get("work_start") or "08:00")
    we = str(c.get("work_end") or "18:00")
    print(("1" if c.get("enabled") else "0"), ws, we)
except Exception:
    pass  # 损坏/缺失: 什么都不打, 调用方按"读不到"处理
PY
}

in_win() {  # "HH:MM" 当前时刻是否在 s~e 窗口内(支持跨零点; 解析失败返回 1)
    local s e n
    s=$(date +%s -d "today $1" 2>/dev/null) || return 1
    e=$(date +%s -d "today $2" 2>/dev/null) || return 1
    [ -n "$s" ] && [ -n "$e" ] || return 1
    n=$(date +%s)
    if [ "$s" -le "$e" ]; then
        [ "$n" -ge "$s" ] && [ "$n" -lt "$e" ]
    else
        [ "$n" -ge "$s" ] || [ "$n" -lt "$e" ]
    fi
}

SET=$(read_cfg)
if [ -z "$SET" ] || [ "${SET%% *}" != "1" ]; then
    # 读不到配置 / enabled=false: 不按作息判定, 照常执行(旧版行为)
    echo "[worktime] schedule 未启用或不可读, 直接执行 $ACT"
    exec systemctl "$ACT" $SVC
fi

WS=$(echo "$SET" | awk '{print $2}')
WE=$(echo "$SET" | awk '{print $3}')

case "$ACT" in
    start)
        if in_win "$WS" "$WE"; then
            echo "[worktime] 当前在工作时段($WS~$WE), 执行 start"
            exec systemctl start $SVC
        fi
        echo "[worktime] 当前不在工作时段($WS~$WE), 跳过补跑的 start(防整夜跑)"
        ;;
    stop)
        if ! in_win "$WS" "$WE"; then
            echo "[worktime] 当前不在工作时段($WS~$WE), 执行 stop"
            exec systemctl stop $SVC
        fi
        echo "[worktime] 当前仍在工作时段($WS~$WE), 跳过补跑的 stop(防白天误停)"
        ;;
    *)
        echo "未知动作: $ACT (只支持 start|stop)" >&2
        exit 2
        ;;
esac
