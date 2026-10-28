#!/bin/sh
# 端到端冒烟测试：起一个**真的**节点，走一遍「开门 → 查码 → 关门」，
# 再把节点打印出来的码解析回来，顺带验一下限速真的生效。
#
# 用法：sh scripts/e2e.sh
#
# 这不是单元测试的替代品：它验的是"编译出来的二进制 + 真实 HTTP + 真实文件"合起来能用，
# 单元测试验的是每条规则。两件事都要有。
set -eu

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/server"

PORT="${PORT:-18765}"
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

echo "== 编译 =="
go build -o "$TMP/knock-hub$EXE" ./cmd/hub || fail "编译失败"
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
  "max_rooms": 10,
  "lookup_per_minute": 8,
  "create_per_minute": 5
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

echo "== 开门 =="
BODY="$(curl -fsS -X POST -d '{"port":25565}' "$BASE/rooms")" || fail "POST /rooms 失败"
CODE="$(printf '%s' "$BODY" | sed -n 's/.*"code":"\([A-Z0-9]*\)".*/\1/p')"
[ "${#CODE}" -eq 6 ] || fail "门牌号不是 6 位：$CODE"
echo "   门牌号：$CODE"

echo "== 查码 =="
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/rooms/$CODE")" = "200" ] || fail "查码应当 200"

echo "== 大小写与 Crockford 别名 =="
LOWER="$(printf '%s' "$CODE" | tr 'A-Z' 'a-z')"
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/rooms/$LOWER")" = "200" ] || fail "小写门牌号应当也能查到"

echo "== 令牌只在开门那一次出现 =="
TOKEN="$(printf '%s' "$BODY" | sed -n 's/.*"token":"\([0-9a-f]*\)".*/\1/p')"
[ -n "$TOKEN" ] || fail "开门响应里没有令牌"
if curl -fsS "$BASE/rooms/$CODE" | grep -q '"token"'; then
	fail "查码响应不该带令牌"
fi

echo "== announce：房主发布身份与候选地址 =="
HOSTID="$(printf '%064d' 0 | tr '0' 'a')"
curl -fsS -X POST \
	-d "{\"token\":\"$TOKEN\",\"host_id\":\"$HOSTID\",\"addrs\":[\"203.0.113.7:5000\",\"203.0.113.7:5001\"]}" \
	"$BASE/rooms/$CODE/announce" >/dev/null || fail "announce 失败"
# 一次取回、两次断言：查码是限速的，别为了 grep 多花配额
curl -fsS "$BASE/rooms/$CODE" >"$TMP/room.json" || fail "announce 后查码失败"
grep -q "203.0.113.7:5000" "$TMP/room.json" || fail "查码里看不到候选地址"
grep -q "$HOSTID" "$TMP/room.json" || fail "查码里看不到房主身份"

echo "== 错令牌必须被拒（403）=="
STATUS="$(curl -s -o /dev/null -w '%{http_code}' -X POST \
	-d "{\"token\":\"deadbeef\",\"host_id\":\"$HOSTID\",\"addrs\":[\"203.0.113.7:5000\"]}" \
	"$BASE/rooms/$CODE/announce")"
[ "$STATUS" = "403" ] || fail "错令牌应当 403，实际 $STATUS"

echo "== 关门 =="
[ "$(curl -s -o /dev/null -w '%{http_code}' -X DELETE "$BASE/rooms/$CODE")" = "204" ] || fail "关门应当 204"
[ "$(curl -s -o /dev/null -w '%{http_code}' "$BASE/rooms/$CODE")" = "404" ] || fail "关门后应当 404"

echo "== 节点码 =="
NODECODE="$("$BIN" -data-dir "$TMP" -print-code | grep -oE '^  [0-9A-Z]{20,}$' | tr -d ' ' | head -1)"
[ -n "$NODECODE" ] || fail "-print-code 没有输出节点码"
echo "   节点码：$NODECODE"
[ "${#NODECODE}" -ge 29 ] || fail "节点码太短：${#NODECODE}"

echo "== 解析完整邀请（同一份 decoder 把编出来的码解回来）=="
OUT="$("$BIN" -inspect "$NODECODE.$CODE")" || fail "-inspect 解析失败"
printf '%s' "$OUT" | grep -q "完整邀请" || fail "没识别成完整邀请"
printf '%s' "$OUT" | grep -q "$CODE" || fail "输出里没有门牌号"
printf '%s' "$OUT" | grep -q "relay.example.com" || fail "输出里没有域名"

echo "== 改坏一个字符必须被拒 =="
LAST="$(printf '%s' "$NODECODE" | tail -c 1)"
if [ "$LAST" = "0" ]; then BAD="${NODECODE%?}1"; else BAD="${NODECODE%?}0"; fi
if "$BIN" -inspect "$BAD.$CODE" >/dev/null 2>&1; then
	fail "改坏末位字符的码不该被接受"
fi

echo "== 限速（配置里是 8 次/分）=="
LIMITED=0
n=0
while [ "$n" -lt 12 ]; do
	STATUS="$(curl -s -o /dev/null -w '%{http_code}' "$BASE/rooms/$(printf '%s' "$CODE" | tr 'A-Z' 'a-z')")"
	if [ "$STATUS" = "429" ]; then
		LIMITED=1
		break
	fi
	n=$((n + 1))
done
[ "$LIMITED" = "1" ] || fail "连敲 12 次都没被限速，说明限速没生效"

echo
echo "✓ 全部通过"
