#!/bin/bash
# announce-net-apply.sh — announce-rs 主机网络配置应用/回滚(root 专用)
# 由 config_web.py 经 sudo 调用; sudoers 白名单仅放行本脚本。
# 动作:
#   status                                  输出: 当前IP/前缀 网关 pending标志
#   apply <ip> <prefix> <gw|-> <dns|->      改静态IP(备份+90s自动回滚保护)
#   confirm                                 确认保留新配置(取消回滚定时器)
#   factory                                 恢复出厂 IP 192.168.31.1/24(直连调试场景)
# 安全: 参数仅允许数字/点/空格, 防止向 interfaces 文件注入 post-up 等 ifupdown 指令。
set -u
IFACE=$(ip route | awk '/^default/{print $5; exit}')
IFACE=${IFACE:-enp1s0}
IFACES_FILE=/etc/network/interfaces
BAK=/etc/network/interfaces.bak
ROLLBACK_TIMER=announce-net-rollback.timer

die() { echo "ERR: $*" >&2; exit 1; }

# 严格白名单: 只允许 0-9 和点(不含空格/换行/字母)
valid_host() { [[ $1 =~ ^[0-9.]+$ ]] || return 1
  local o; for o in ${1//./ }; do [ -n "$o" ] && [ "$o" -le 255 ] 2>/dev/null || return 1; done
  return 0; }
# DNS: 数字/点/空格(可多个); 正则含空格必须经变量传递, 否则 [[ =~ ]] 解析失败
valid_dns() { local re='^[0-9. ]+$'; [[ $1 =~ $re ]] || return 1; }

cmd=${1:-}
case "$cmd" in
  status)
    ip -4 addr show dev "$IFACE" scope global | awk '/inet /{print $2}' | head -1
    ip route | awk '/^default/{print $3; exit}'
    if systemctl list-timers "$ROLLBACK_TIMER" --no-legend 2>/dev/null | grep -q .; then
      echo PENDING
    fi
    ;;
  apply)
    IP=${2:-}; PREFIX=${3:-}; GW=${4:--}; DNS=${5:--}
    valid_host "$IP" || die "IP 非法: $IP"
    [[ $PREFIX =~ ^[0-9]+$ && $PREFIX -ge 8 && $PREFIX -le 32 ]] || die "前缀非法: $PREFIX"
    [ "$GW" = "-" ] || valid_host "$GW" || die "网关非法: $GW"
    [ "$DNS" = "-" ] || valid_dns "$DNS" || die "DNS 非法(仅允许 IP 列表): $DNS"
    [ "$GW" != "-" ] && [ "$GW" = "$IP" ] && die "网关不能等于本机 IP"
    # 有未确认的修改时拒绝叠加, 防止把坏配置备份成回滚点
    if systemctl list-timers "$ROLLBACK_TIMER" --no-legend 2>/dev/null | grep -q .; then
      die "上一次修改还在 90 秒确认期内, 请等回滚或先 confirm"
    fi
    # 网关与 IP 同网段校验(靶机有 python3)
    if [ "$GW" != "-" ]; then
      R=$(python3 -c "import sys,ipaddress;i=ipaddress.ip_interface(sys.argv[1]+'/'+sys.argv[2]);print('OK' if ipaddress.ip_address(sys.argv[3]) in i.network else 'BAD')" "$IP" "$PREFIX" "$GW") || die "网段校验执行失败"
      [ "$R" = "OK" ] || die "网关与 IP 不在同一网段"
    fi
    cp "$IFACES_FILE" "$BAK" || die "备份 $IFACES_FILE 失败"
    {
      echo "source /etc/network/interfaces.d/*"
      echo "auto lo"
      echo "iface lo inet loopback"
      echo ""
      echo "allow-hotplug $IFACE"
      echo "iface $IFACE inet static"
      echo "    address $IP/$PREFIX"
      # 注意用 if 而非 [ ] && echo: 后者在条件为假时让 { } 组退出码变 1, 被 || die 误杀
      if [ "$GW" != "-" ]; then echo "    gateway $GW"; fi
      if [ "$DNS" != "-" ]; then echo "    dns-nameservers $DNS"; fi
    } > "$IFACES_FILE" || die "写入配置失败"
    systemctl restart networking || die "networking 重启失败(90秒后将自动回滚)"
    systemd-run --on-active=90 --unit=announce-net-rollback \
      /bin/sh -c "cp $BAK $IFACES_FILE && systemctl restart networking" >/dev/null \
      || die "回滚定时器创建失败(请手动检查网络)"
    echo "OK ip=$IP/$PREFIX gw=$GW dns=$DNS rollback=90s"
    ;;
  confirm)
    systemctl stop "$ROLLBACK_TIMER" 2>/dev/null || true
    echo "CONFIRMED"
    ;;
  factory)
    exec "$0" apply 192.168.31.1 24 - -
    ;;
  *)
    die "用法: $0 status | apply <ip> <prefix> <gw|-> <dns|-> | confirm | factory"
    ;;
esac
