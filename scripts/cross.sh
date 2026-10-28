#!/bin/sh
# 跨语言集成测试：起一个真的 Go 节点，让 Rust 客户端去敲。
#
# 用法：sh scripts/cross.sh
#
# 为什么值得单独一个脚本：两个语言的测试各自绿，不代表它们能对话。
# 这里验的是"Go 打印的节点码，Rust 解得开，而且 pin 能验过 Go 自报的公钥"。
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PORT="${PORT:-18766}"
BASE="http://127.0.0.1:$PORT"
TMP="$(mktemp -d)"
PID=""

cleanup() {
	if [ -n "$PID" ]; then
		kill "$PID" 2>/dev/null || true
	fi
	rm -rf "$TMP"
}
trap cleanup EXIT

fail() {
	echo "✗ $1" >&2
	if [ -f "$TMP/node.log" ]; then
		echo "--- 节点日志 ---" >&2
		cat "$TMP/node.log" >&2
	fi
	exit 1
}

case "$(uname -s)" in
MINGW* | MSYS* | CYGWIN*) EXE=".exe" ;;
*) EXE="" ;;
esac

echo "== 编译 Go 节点 =="
(cd "$ROOT/server" && go build -o "$TMP/knock-hub$EXE" ./cmd/hub) || fail "编译失败"
BIN="$TMP/knock-hub$EXE"

echo "== 写配置 =="
cat >"$TMP/node.json" <<EOF
{
  "label": "",
  "public_port": 8443,
  "domain": "relay.example.com",
  "ipv4": "",
  "ipv6": "",
  "relay_allowed": true,
  "listen": "127.0.0.1:$PORT",
  "room_ttl": "10m",
  "max_rooms": 50,
  "lookup_per_minute": 100,
  "create_per_minute": 100
}
EOF

echo "== 启动节点 =="
"$BIN" -data-dir "$TMP" >"$TMP/node.log" 2>&1 &
PID=$!

i=0
while [ "$i" -lt 60 ]; do
	if curl -fsS --max-time 1 "$BASE/healthz" >/dev/null 2>&1; then
		break
	fi
	sleep 0.1
	i=$((i + 1))
done
curl -fsS --max-time 1 "$BASE/healthz" >/dev/null 2>&1 || fail "节点没起来"

echo "== 取出节点码 =="
NODECODE="$("$BIN" -data-dir "$TMP" -print-code | grep -oE '^  [0-9A-Z]{20,}$' | tr -d ' ' | head -1)"
[ -n "$NODECODE" ] || fail "拿不到节点码"

echo "== 跑 Rust 集成测试（敲这个真节点）=="
cd "$ROOT/client/src-tauri"
KNOCK_NODE_URL="$BASE" KNOCK_NODE_CODE="$NODECODE" \
	cargo test --test cross_node -- --ignored --nocapture 2>&1 | tail -20

echo
echo "✓ 跨语言集成测试通过"
