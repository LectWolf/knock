#!/bin/sh
# 传输层端到端：真节点 + 真中继 + 两个真端点。
#
# 用法：
#   sh scripts/transport.sh
#   KNOCK_NODE_URL=... KNOCK_NODE_CODE=... KNOCK_RELAY_URL=... sh scripts/transport.sh
#
# 四项：直连跑通、只有门没人、码不存在、打洞失败落到中继。
set -eu

NODE_URL="${KNOCK_NODE_URL:-http://154.36.165.162:30990}"
NODE_CODE="${KNOCK_NODE_CODE:-377PN6H4MPH0WYEFD838Q42C674TR}"
RELAY_URL="${KNOCK_RELAY_URL:-http://127.0.0.1:30991}"

echo "节点：$NODE_URL"
echo "中继：$RELAY_URL（默认走本机隧道，见 ssh -L 30991:127.0.0.1:30991）"

curl -fsS --max-time 5 "$NODE_URL/healthz" >/dev/null || {
	echo "✗ 节点不可达" >&2
	exit 1
}

cd "$(dirname "$0")/../client/src-tauri"
KNOCK_NODE_URL="$NODE_URL" KNOCK_NODE_CODE="$NODE_CODE" KNOCK_RELAY_URL="$RELAY_URL" \
	cargo test --test transport_e2e -- --ignored --nocapture

echo
echo "✓ 传输层全部通过"
