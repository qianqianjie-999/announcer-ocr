#!/bin/bash
# audio_health.sh — 播报音频链路自检/自愈(2026-10-22)
# 背景: ALC662 声卡在 HDA 省电/冷启动/插拔后会"叫不醒", aplay 正常返回或直接失败,
#       现场表现就是"该播报但没声音"。识别错一两个可以接受, 哑火不可接受。
# 策略:
#   ① 每 60s 扫 service.log: 出现 "aplay 播放失败" 立即恢复
#   ② 每 5 分钟做一次静音自检(设备空闲才做, 避免抢设备丢掉真实播报)
#   ③ 恢复顺序: alsactl restore + 混音器复位 → aplay 复测
#                → [USB 声卡] 设备热复位(unbind/bind, ~2s, 可无限次) → aplay 复测
#                → 重启 announce-rs → aplay 复测
#   ④ 仍无声且连续 2 轮独立确认 → 整机重启(现场已验证"重启 = 插拔 3.5mm"且更彻底:
#      冷启动会重载 snd_hda_intel 并让 power_save_controller=N 生效)。
#      限流: 30 分钟 1 次, 连续 3 次仍无声就停手(防音箱没开电源导致无限重启)。
#
# USB 声卡(2026-10-22 新增): 把"不可软件复位的板载 HDA"换成"可软件复位的 USB 设备"。
#   板载 ALC662 一旦 codec 状态机死锁, 软件唯一的复位手段就是整机重启(60s 空窗, 且有上限);
#   USB 设备可以 unbind/bind 热复位, 2 秒完成、可无限次 —— 这才是根治。
#   配套必须关 USB autosuspend, 否则只是把"HDA 省电掉线"换成"USB 自动挂起掉线"。
set -u
LOG=/var/log/announce-rs/audio_health.log
STATE=/var/lib/announce-rs
SIL=/opt/announce-rs/silence.wav
DEV=${AUDIO_DEV:-plughw:0,0}
DRY=${AUDIO_DRY:-0}          # 演练模式: 只测不救, 不重启服务
PCM=${AUDIO_PCM:-}          # 占用检测用的 status 文件; 缺省按 AUDIO_DEV 推导卡号(见 aplay_test), 不再写死 card0
REBOOT_GAP=${AUDIO_REBOOT_GAP:-1800}   # 两次整机重启最小间隔
REBOOT_MAX=${AUDIO_REBOOT_MAX:-3}      # 连续整机重启上限(超了只告警)
mkdir -p "$STATE"

ts() { date '+%F %T'; }
say() { echo "$(ts) $*" >> "$LOG"; }

# 日志超过 2000 行就截断(现场 SD 卡, 别让它长)
[ -f "$LOG" ] && [ "$(wc -l < "$LOG")" -gt 2000 ] && tail -1000 "$LOG" > "$LOG.tmp" && mv "$LOG.tmp" "$LOG"

# 静音样本缺失就现场生成(自愈, 免得因为一个 wav 丢了整套监测)
if [ ! -f "$SIL" ]; then
    python3 - "$SIL" <<'PYEOF'
import sys, wave
w = wave.open(sys.argv[1], 'wb')
w.setnchannels(1); w.setsampwidth(2); w.setframerate(16000)
w.writeframes(b'\x00\x00' * 3200)
w.close()
PYEOF
    say "WARN 静音样本缺失, 已重新生成"
fi

aplay_test() {
    # 设备被播报占用时不要抢: 直接视为"健康"(有占用说明链路在用)
    # 占用状态文件跟随 AUDIO_DEV 推导的卡号(写死 card0 时, 换 USB 声卡后读错卡)。
    # 卡号推导失败(查不到卡/default)就跳过占用守卫直接试放——aplay 自身的失败
    # 会触发升级链, 不静默放行。
    local pcm=""
    if [ -n "${PCM:-}" ]; then pcm="$PCM"
    elif card=$(audio_card_num); then pcm="/proc/asound/card$card/pcm0p/sub0/status"
    fi
    if [ -n "$pcm" ] && [ -r "$pcm" ] && ! grep -q closed "$pcm" 2>/dev/null; then
        say "SKIP(设备占用中, 视为在用)"
        return 0
    fi
    timeout 8 aplay -q -D "$DEV" "$SIL" 2>>"$LOG"
}

reset_mixer() {
    alsactl restore -f /var/lib/alsa/asound.state 2>>"$LOG" || true
    if card=$(audio_card_num); then
        amixer -c "$card" sset 'Auto-Mute Mode' Disabled >/dev/null 2>&1 || true
        amixer -c "$card" sset Master 100% unmute    >/dev/null 2>&1 || true
        amixer -c "$card" sset Headphone unmute      >/dev/null 2>&1 || true
    else
        say "WARN 无法从 AUDIO_DEV($DEV) 推导卡号, 跳过 amixer 恢复(alsactl 已做全卡恢复)"
    fi
}

# ---- USB 声卡支持: 定位 / 热复位 / autosuspend 体检 ----
# 从声卡 sysfs 往上回溯, 找到形如 "1-2" 的 USB 设备节点(不是 "1-2:1.0" 那个 interface)。
# 路径可覆盖(TEST 用: AUDIO_SYSFS=/tmp/fakeroot 就能在本机跑真实逻辑)
SYSFS_ROOT=${AUDIO_SYSFS:-/sys}
CARDS_FILE=${AUDIO_CARDS:-/proc/asound/cards}

usb_syspath() {
    local card="${1:-0}" p b
    p=$(readlink -f "${SYSFS_ROOT}/class/sound/controlC${card}/device" 2>/dev/null) || return 1
    [ -n "$p" ] || return 1
    while [ "$p" != "/" ]; do
        b=$(basename "$p")
        case "$b" in
            [0-9]*-[0-9]*)          # 形如 1-2 / 2-1.3, 即 USB 设备节点
                case "$b" in
                    *:*) ;;         # 1-2:1.0 是 interface, 再往上一层才是设备
                    *) echo "$p"; return 0 ;;
                esac ;;
        esac
        p=$(dirname "$p")
    done
    return 1
}

# 当前主用声卡的卡号: 支持 plughw:0,0(取数字) 和 plughw:CARD=Device,DEV=0(按名字查卡号)
audio_card_num() {
    case "${DEV}" in
        *CARD=*)
            local name line
            name=$(echo "$DEV" | sed -n 's/.*CARD=\([^,]*\).*/\1/p')
            line=$(grep -i -m1 "\[${name}" "$CARDS_FILE" 2>/dev/null)
            if [ -n "$line" ]; then echo "$line" | awk '{print $1}'; return 0; fi
            return 1 ;;   # 查不到卡: 明确失败(回退 card0 会读错卡, 正是 2026-10-22 修的 bug 的残留路径)
        *)
            local n
            n=$(echo "$DEV" | grep -oE '[0-9]+' | head -1)
            [ -n "$n" ] && { echo "$n"; return 0; }
            return 1 ;;   # default/无数字写法: 推导不出卡号, 由调用方决定降级
    esac
}

# 热复位 USB 声卡:  equivalent 于"拔掉再插上", 但 2 秒完成、无人到场、可无限次。
usb_reset() {
    local card p bus
    card=$(audio_card_num) || { say "ERROR AUDIO_DEV($DEV) 推导不出卡号, 无法定位 USB 节点"; return 1; }
    p=$(usb_syspath "$card") || { say "USB-RESET 跳过: card$card 不是 USB 设备(板载 HDA)"; return 1; }
    bus=$(basename $(dirname $p))/$(basename $p)   # 形如 usb1/1-2
    say "USB-RESET 复位 $bus (card$card)"
    echo -n "$(basename $p)" > /sys/bus/usb/drivers/usb/unbind 2>>"$LOG" \
        || { say "ERROR unbind 失败(权限/设备已消失), 升级下一级恢复"; return 1; }
    sleep 2
    echo -n "$(basename $p)" > /sys/bus/usb/drivers/usb/bind 2>>"$LOG" \
        || { say "ERROR bind 失败(复位不完整), 升级下一级恢复"; return 1; }
    sleep 3
    # 重新枚举后卡号可能漂移 -> 若原 DEV 不可用且存在新卡, 记一条提示由人工改 AUDIO_DEV
    aplay -l 2>/dev/null | grep -q '^card' || say "CRITICAL 复位后 aplay -l 无设备, 检查 USB 连接"
    return 0
}

# USB autosuspend 体检: 开着就等于埋了个新的"省电掉线", 必须关。
usb_autosuspend_check() {
    local card=$(audio_card_num) p
    p=$(usb_syspath "$card") || return 0
    [ -w "$p/power/control" ] || return 0
    local ctl=$(cat "$p/power/control" 2>/dev/null)
    if [ "$ctl" != "on" ]; then
        echo on > "$p/power/control" 2>/dev/null
        echo -1 > "$p/power/autosuspend_delay_ms" 2>/dev/null
        say "WARN USB autosuspend 处于 '$ctl' -> 已强制置 on(防自动挂起掉线)"
    fi
}

recover() {
    local why="$1"
    say "RECOVER 触发: $why"
    reset_mixer
    sleep 1
    if aplay_test; then say "RECOVER-OK(混音器复位)"; healthy; return 0; fi
    if [ "$DRY" = "1" ]; then
        say "DRY 演练: 混音器复位无效, 正常流程此处会 USB 热复位 / 重启 announce-rs"
        return 1
    fi
    # USB 声卡: 热复位优先(2 秒, 可无限次), 比整机重启便宜得多
    if usb_reset; then
        sleep 2
        if aplay_test; then say "RECOVER-OK(USB 热复位)"; healthy; return 0; fi
        say "USB 热复位无效, 继续升级"
    fi
    say "混音器/USB 复位均无效, 重启 announce-rs"
    systemctl restart announce-rs
    sleep 10
    if aplay_test; then say "RECOVER-OK(重启服务)"; healthy; return 0; fi

    local n=$(( $(cat "$STATE/audio_fail_streak" 2>/dev/null || echo 0) + 1 ))
    echo "$n" > "$STATE/audio_fail_streak"
    say "CRITICAL 重启服务后仍无声 (连续失败=$n)"

    local now=$(date +%s)
    local rb=0 lastrb=0
    [ -f "$STATE/audio_reboot_total" ] && rb=$(cat "$STATE/audio_reboot_total")
    [ -f "$STATE/audio_last_reboot" ] && lastrb=$(cat "$STATE/audio_last_reboot")

    if [ "${rb:-0}" -ge "$REBOOT_MAX" ]; then
        say "CRITICAL 已自动重启 $rb 次仍无声 -> 停止重启, 请人工检查(音箱电源 / 3.5mm 线 / 声卡)"
        return 1
    fi
    if [ "$n" -lt 2 ]; then
        say "待下一轮独立确认(连续失败 $n/2), 仍无声则重启整机"
        return 1
    fi
    if [ $(( now - lastrb )) -lt "$REBOOT_GAP" ]; then
        say "距上次整机重启不足 $(( REBOOT_GAP / 60 )) 分钟, 本轮不重复重启"
        return 1
    fi

    echo "$now" > "$STATE/audio_last_reboot"
    echo $(( rb + 1 )) > "$STATE/audio_reboot_total"
    say "ESCALATE 连续 $n 轮无声 -> 60 秒后整机重启 (第 $(( rb + 1 ))/$REBOOT_MAX 次, 等同插拔 3.5mm 且更彻底)"
    if systemd-run --on-active=60 --unit=audio-reboot /sbin/reboot >/dev/null 2>&1; then
        return 1
    fi
    # systemd-run 排程失败(罕见)就放弃自动重启, 绝不退回 (sleep 60 && reboot) & 裸子 shell:
    # 那种重启 healthy() 取消不了 —— "不可取消的重启"比"暂不重启"更危险, 打 ERROR 留给人工。
    say "ERROR systemd-run 排程整机重启失败, 已放弃本轮自动重启(可取消的方案才安全), 请人工介入"
    return 1
}

# 音频正常时: 清计数, 并取消"已排上队但还没执行"的整机重启
healthy() {
    echo 0 > "$STATE/audio_fail_streak"
    echo 0 > "$STATE/audio_reboot_total"
    if systemctl is-active --quiet audio-reboot.timer 2>/dev/null; then
        systemctl stop audio-reboot.timer >/dev/null 2>&1 || true
        say "音频已自行恢复 -> 取消待执行的整机重启"
    fi
}

# ---- ① 播报失败即时响应 ----
CNT=$(grep -c 'aplay 播放失败' /var/log/announce-rs/service.log 2>/dev/null || true)
CNT=${CNT:-0}
PREV=$(cat "$STATE/aplay_err_cnt" 2>/dev/null || true)
PREV=${PREV:-$CNT}
echo "$CNT" > "$STATE/aplay_err_cnt"
if [ "$CNT" -gt "$PREV" ]; then
    recover "service.log 新增 $(($CNT - $PREV)) 条 aplay 播放失败"
    exit 0
fi

# ---- ② 定时静音自检(每 5 分钟) ----
MARK="$STATE/audio_last_probe"
if [ -f "$MARK" ] && [ $(( $(date +%s) - $(cat "$MARK") )) -lt 300 ]; then
    exit 0
fi
date +%s > "$MARK"

# piper 完整性(合成失败=没声, 无法自愈, 只告警)
if [ ! -x /opt/piper/piper/piper ] || [ ! -f /opt/piper/piper/zh_CN-huayan-medium.onnx ]; then
    say "CRITICAL piper 或语音模型缺失, 播报将无声"
fi

# USB 声卡: autosuspend 开着 = 换个花样掉线, 每次自检顺手纠正
usb_autosuspend_check
# 板载 HDA 仍在用且已确认过是故障源 -> 提醒可以换 USB
if ! card=$(audio_card_num) || ! usb_syspath "$card" >/dev/null 2>&1; then
    H=$(date '+%m%d')
    if [ "$(cat "$STATE/hda_warn_day" 2>/dev/null)" != "$H" ]; then
        echo "$H" > "$STATE/hda_warn_day"
        say "INFO 当前声卡不可 USB 热复位(card=${card:-未知}), codec 死锁只能靠整机重启复位; 换 USB 声卡可 2s 热复位"
    fi
fi

if aplay_test; then
    healthy
    H=$(date '+%m%d%H')
    if [ "$(cat "$STATE/last_ok_hour" 2>/dev/null)" != "$H" ]; then
        echo "$H" > "$STATE/last_ok_hour"
        say "OK 每小时自检通过"
    fi
    exit 0
fi
recover "定时自检 aplay 失败"
