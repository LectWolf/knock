#!/bin/sh
# Knock 节点一条命令安装脚本（Linux）。
#
#   curl -fsSL https://raw.githubusercontent.com/OWNER/knock/main/deploy/install.sh | sh
#
# 常用变体：
#   sh install.sh --domain relay.example.com --port 8443
#   sh install.sh --ip 203.0.113.7 --no-relay
#   sh install.sh --binary ./bin/knock-hub        # 用本地二进制，不下载
#
# 只做三件事：装二进制、写配置、起 systemd。不做别的、不装依赖。
set -eu

DATA_DIR=/var/lib/knock
BIN_PATH=/usr/local/bin/knock-hub
UNIT_PATH=/etc/systemd/system/knock-node.service
REPO="${KNOCK_REPO:-OWNER/knock}"
VERSION="${KNOCK_VERSION:-latest}"

DOMAIN=""
IP=""
PORT="8443"
RELAY="true"
LOCAL_BINARY=""

usage() {
	cat <<'EOF'
Knock 节点安装脚本

参数：
  --domain <域名>    节点对外域名（推荐：IP 变了不用重发节点码）
  --ip <地址>        节点对外 IPv4/IPv6（没有域名时用）
  --port <端口>      对外端口，默认 8443
  --no-relay         只做信令，不替别人转发流量
  --binary <路径>    用本地编译好的 knock-hub，不联网下载
  --data-dir <路径>  数据目录，默认 /var/lib/knock
  -h, --help         这个帮助

装完会把节点码打印出来，那一串就是别人要粘贴的东西。
EOF
}

while [ $# -gt 0 ]; do
	case "$1" in
	--domain) DOMAIN="${2:?--domain 需要值}"; shift 2 ;;
	--ip) IP="${2:?--ip 需要值}"; shift 2 ;;
	--port) PORT="${2:?--port 需要值}"; shift 2 ;;
	--no-relay) RELAY="false"; shift ;;
	--binary) LOCAL_BINARY="${2:?--binary 需要路径}"; shift 2 ;;
	--data-dir) DATA_DIR="${2:?--data-dir 需要路径}"; shift 2 ;;
	-h | --help) usage; exit 0 ;;
	*) echo "不认识的参数：$1" >&2; usage >&2; exit 2 ;;
	esac
done

if [ "$(id -u)" -ne 0 ]; then
	echo "需要 root 才能装成系统服务。请用 sudo 重新执行。" >&2
	exit 1
fi

if ! command -v systemctl >/dev/null 2>&1; then
	echo "没有 systemd，这个脚本不适用。可以直接跑：knock-hub -data-dir $DATA_DIR" >&2
	exit 1
fi

# —— 1. 放二进制 ——
if [ -n "$LOCAL_BINARY" ]; then
	[ -f "$LOCAL_BINARY" ] || { echo "找不到 $LOCAL_BINARY" >&2; exit 1; }
	install -m 0755 "$LOCAL_BINARY" "$BIN_PATH"
	echo "已安装本地二进制：$LOCAL_BINARY"
else
	case "$(uname -m)" in
	x86_64 | amd64) ARCH=amd64 ;;
	aarch64 | arm64) ARCH=arm64 ;;
	*) echo "不支持的架构：$(uname -m)" >&2; exit 1 ;;
	esac

	if [ "$VERSION" = "latest" ]; then
		URL="https://github.com/$REPO/releases/latest/download/knock-hub-linux-$ARCH"
	else
		URL="https://github.com/$REPO/releases/download/$VERSION/knock-hub-linux-$ARCH"
	fi

	echo "下载：$URL"
	TMP="$(mktemp)"
	# -f 让 404 直接失败，而不是把一段 HTML 装成二进制
	if ! curl -fsSL "$URL" -o "$TMP"; then
		echo "下载失败。项目还没发 release 的话，用 --binary 指向本地编译的二进制。" >&2
		rm -f "$TMP"
		exit 1
	fi
	install -m 0755 "$TMP" "$BIN_PATH"
	rm -f "$TMP"
fi

# —— 2. 写配置（已存在就不动）——
mkdir -p "$DATA_DIR"
chmod 700 "$DATA_DIR"

CONFIG="$DATA_DIR/node.json"
if [ ! -f "$CONFIG" ]; then
	if [ -z "$DOMAIN" ] && [ -z "$IP" ]; then
		# 没给地址就试着问一下公网 IP；失败也不拦着，让人自己填
		IP="$(curl -4 -fsS --max-time 5 https://api.ipify.org 2>/dev/null || true)"
		if [ -z "$IP" ]; then
			IP="$(curl -6 -fsS --max-time 5 https://api64.ipify.org 2>/dev/null || true)"
		fi
		[ -n "$IP" ] && echo "没指定地址，探测到公网 IP：$IP"
	fi

	cat >"$CONFIG" <<EOF
{
  "label": "",
  "public_port": $PORT,
  "domain": "$DOMAIN",
  "ipv4": "$IP",
  "ipv6": "",
  "relay_allowed": $RELAY,
  "listen": ":$PORT",
  "room_ttl": "10m",
  "max_rooms": 200
}
EOF
	chmod 600 "$CONFIG"
	echo "已写配置：$CONFIG"
else
	echo "配置已存在，保持不动：$CONFIG"
fi

# —— 3. systemd ——
if [ -f "$(dirname "$0")/knock-node.service" ]; then
	install -m 0644 "$(dirname "$0")/knock-node.service" "$UNIT_PATH"
else
	# 从管道执行时拿不到同目录文件，内联一份
	cat >"$UNIT_PATH" <<'EOF'
[Unit]
Description=Knock 节点（房间登记；中继由 iroh-relay 承担）
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/knock-hub -data-dir /var/lib/knock
Restart=on-failure
RestartSec=3s
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/var/lib/knock
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
EOF
fi

systemctl daemon-reload
systemctl enable --now knock-node.service

# —— 4. 把节点码亮出来 ——
echo
if ! "$BIN_PATH" -data-dir "$DATA_DIR" -print-code; then
	echo "节点码还没生成成功：先把 $CONFIG 里的地址填上，然后" >&2
	echo "  systemctl restart knock-node && journalctl -u knock-node -n 40" >&2
	exit 1
fi

echo
echo "服务状态：systemctl status knock-node"
echo "看日志：  journalctl -u knock-node -f"
