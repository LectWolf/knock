#!/usr/bin/env bash
# Knock 节点安装脚本（Debian/Ubuntu + systemd）
#
# 交互安装：  sudo ./install.sh
# 全自动：    sudo ./install.sh --yes --name 枣庄-14900K --addr 202.189.15.149 --port 30990 --relay
#
# 关键设计：
#   * 对外**只开一个端口**：房间登记 +（可选）数据中转共用一个，
#     中继本身只监听 127.0.0.1，由 knock-hub 按路径转进来。
#   * 装完 systemd 自启动，掉线自动拉起。
#   * 额外装一个 `knock` 管理命令（不带参数就是菜单）。
set -euo pipefail

DATA_DIR="${KNOCK_DATA_DIR:-/var/lib/knock}"
BIN_DIR="${KNOCK_BIN_DIR:-/usr/local/bin}"
NODE_UNIT=/etc/systemd/system/knock-node.service
RELAY_UNIT=/etc/systemd/system/knock-relay.service
RELAY_TOML=/etc/knock-relay.toml

C_ACC=$'\033[38;5;179m'; C_OK=$'\033[32m'; C_WARN=$'\033[33m'; C_MUT=$'\033[90m'; C_OFF=$'\033[0m'
ok()   { printf '%s✓%s %s\n' "$C_OK" "$C_OFF" "$*"; }
warn() { printf '%s！%s %s\n' "$C_WARN" "$C_OFF" "$*"; }
dim()  { printf '%s%s%s\n' "$C_MUT" "$*" "$C_OFF"; }

NAME=""; ADDR=""; PORT=""; DOMAIN=""; RELAY=""; BUNDLE=""; BASE_URL=""; YES=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --name) NAME="$2"; shift 2;;
    --addr|--ip) ADDR="$2"; shift 2;;
    --port) PORT="$2"; shift 2;;
    --domain) DOMAIN="$2"; shift 2;;
    --relay) RELAY=1; shift;;
    --no-relay) RELAY=0; shift;;
    --bundle) BUNDLE="$2"; shift 2;;
    --url) BASE_URL="$2"; shift 2;;
    --binary) BUNDLE="$(dirname "$2")"; shift 2;;
    --data-dir) DATA_DIR="$2"; shift 2;;
    --yes|-y) YES=1; shift;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0;;
    *) warn "不认识的参数：$1"; exit 2;;
  esac
done

[[ ${EUID:-0} -eq 0 ]] || { warn "请用 root 跑（sudo）"; exit 1; }
command -v systemctl >/dev/null || { warn "需要 systemd（Debian/Ubuntu）"; exit 1; }

ask() { # ask <问题> <默认>
  local q="$1" def="${2:-}" ans=""
  if (( YES )); then printf '%s' "$def"; return; fi
  printf '%s%s%s [%s] ' "$C_ACC" "$q" "$C_OFF" "$def"
  read -r ans || true
  printf '%s' "${ans:-$def}"
}
ask_yn() { # ask_yn <问题> <默认 y|n>
  local q="$1" def="${2:-y}" ans=""
  if (( YES )); then [[ $def == y ]]; return; fi
  printf '%s%s%s [%s] ' "$C_ACC" "$q" "$C_OFF" "$([[ $def == y ]] && echo 'Y/n' || echo 'y/N')"
  read -r ans || true
  [[ "${ans:-$def}" =~ ^[Yy] ]]
}

echo
dim "正在识别系统…"
. /etc/os-release 2>/dev/null || true
ok "${PRETTY_NAME:-Linux} · $(uname -m) · $(nproc) 核"
ok "systemd 可用"
command -v curl >/dev/null && ok "curl 可用" || { warn "先装 curl：apt install -y curl"; exit 1; }
echo

[[ -n "$NAME" ]] || NAME="$(ask "节点叫什么名字？" "node-$(hostname -s)")"
if [[ -z "$ADDR" ]]; then
  DETECTED="$(curl -fsS --max-time 4 https://api.ipify.org 2>/dev/null || true)"
  ADDR="$(ask "别人连这个节点用哪个地址？" "${DETECTED:-}")"
fi
[[ -n "$PORT" ]] || PORT="$(ask "对外开放端口？" "30990")"
if [[ -z "$RELAY" ]]; then
  ask_yn "要带「数据中转」吗？直连打不通时可以让流量绕这里。" y && RELAY=1 || RELAY=0
fi

[[ "$ADDR" =~ ^[A-Za-z0-9.:\ -]+$ ]] || { warn "地址不对：$ADDR"; exit 1; }
[[ "$PORT" =~ ^[0-9]+$ ]] && (( PORT > 0 && PORT < 65536 )) || { warn "端口不对：$PORT"; exit 1; }

SCHEME=http; RELAY_URL=""; ALLOW_PLAIN=false; RELAY_LOCAL=""
if (( RELAY )); then
  if [[ -n "$DOMAIN" ]]; then SCHEME=https
  else
    dim "  好，中继和房间服务共用 $PORT 这一个端口（明文 http；要 https 用 --domain 配反代证书）"
    ALLOW_PLAIN=true
  fi
  RELAY_URL="$SCHEME://$ADDR:$PORT"
  RELAY_LOCAL=127.0.0.1:30991
fi
echo

fetch_bin() { # fetch_bin <源文件名> <目标>
  local src="$1" dst="$2"
  if [[ -n "$BUNDLE" ]]; then
    [[ -f "$BUNDLE/$src" ]] || { warn "$BUNDLE 里没有 $src"; exit 1; }
    install -m 755 "$BUNDLE/$src" "$dst"
  elif [[ -n "$BASE_URL" ]]; then
    curl -fsSL --max-time 180 "$BASE_URL/$src" -o "$dst.tmp"
    install -m 755 "$dst.tmp" "$dst"; rm -f "$dst.tmp"
  else
    warn "没有二进制来源：加 --bundle DIR 或 --url BASE"; exit 1
  fi
}

dim "安装组件…"
mkdir -p "$DATA_DIR"
fetch_bin knock-hub-linux "$BIN_DIR/knock-hub"
ok "已安装 $BIN_DIR/knock-hub"
if (( RELAY )); then
  fetch_bin iroh-relay "$BIN_DIR/iroh-relay"
  ok "已安装 $BIN_DIR/iroh-relay"
fi

cat > "$DATA_DIR/node.json" <<JSON
{
  "public_port": $PORT,
  "domain": "$DOMAIN",
  "ipv4": "$ADDR",
  "ipv6": "",
  "relay_allowed": $([[ $RELAY == 1 ]] && echo true || echo false),
  "relay_url": "$RELAY_URL",
  "relay_local": "$RELAY_LOCAL",
  "allow_plain_relay": $ALLOW_PLAIN,
  "listen": ":$PORT",
  "room_ttl": "10m",
  "max_rooms": 200,
  "lookup_per_minute": 30,
  "create_per_minute": 10,
  "name": "$NAME"
}
JSON
ok "配置写到 $DATA_DIR/node.json"

if (( RELAY )); then
  cat > "$RELAY_TOML" <<'TOML'
# Knock 中继：只听环回，对外由 knock-hub 那一个端口转进来
enable_relay = true
http_bind_addr = "127.0.0.1:30991"
enable_quic_addr_discovery = false
TOML
  cat > "$RELAY_UNIT" <<UNIT
[Unit]
Description=Knock relay (iroh-relay, loopback only)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$BIN_DIR/iroh-relay --dev --config-path $RELAY_TOML
Restart=always
RestartSec=3

[Install]
WantedBy=multi-user.target
UNIT
fi

cat > "$NODE_UNIT" <<UNIT
[Unit]
Description=Knock node (rooms$([[ $RELAY == 1 ]] && echo ' + relay' || true))
After=network-online.target
Wants=network-online.target
$( [[ $RELAY == 1 ]] && echo 'After=knock-relay.service' )

[Service]
Type=simple
WorkingDirectory=$DATA_DIR
ExecStart=$BIN_DIR/knock-hub -config $DATA_DIR/node.json -data-dir $DATA_DIR
Restart=always
RestartSec=3
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
UNIT

if [[ -f "$(dirname "$0")/knock" ]]; then
  install -m 755 "$(dirname "$0")/knock" "$BIN_DIR/knock"
  ok "已安装管理命令 $BIN_DIR/knock"
fi

systemctl daemon-reload
if (( RELAY )); then systemctl enable --now knock-relay >/dev/null 2>&1 || true; fi
systemctl enable --now knock-node >/dev/null 2>&1 || true
sleep 2

echo
dim "启动服务…"
systemctl is-active --quiet knock-node || { warn "没起来，看 journalctl -u knock-node -n 30"; exit 1; }
ok "knock-node.service — active (running)"
(( RELAY )) && systemctl is-active --quiet knock-relay && ok "knock-relay.service — active (running)  监听 127.0.0.1:30991（只给本机用）"

echo
dim "—— 这个节点的编码 ——"
CODE="$("$BIN_DIR/knock-hub" -config "$DATA_DIR/node.json" -data-dir "$DATA_DIR" -print-code 2>/dev/null | tr -d '[:space:]' || true)"
if [[ -n "$CODE" ]]; then
  dim "节点码（复制这一整串发给别人）："
  printf '\n  %s%s%s\n\n' "$C_ACC" "$CODE" "$C_OFF"
else
  warn "节点码没打出来：journalctl -u knock-node | grep -A2 节点码"
fi
warn "记得在云厂商安全组放行 TCP ${PORT}（只需要这一个）"
echo
ok "装好了。以后用 knock 管它（直接输 knock 就是菜单）"
echo
