package invite_test

import (
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"os"
	"strconv"
	"strings"
	"testing"

	"knock/internal/invite"
)

// 与 Rust 共用的金标向量。任何一边改了布局、CRC、base32 或分隔符，这里都会红。
const vectorsPath = "../../../spec/invite-v1.tsv"

type vector struct {
	name        string
	label       uint16
	ipv4        [4]byte
	hasIPv4     bool
	ipv6        [16]byte
	hasIPv6     bool
	domain      string
	port        uint16
	pin         [8]byte
	relay       bool
	bytesHex    string
	code        string
	room        string
	fullInvite  string
	shortInvite string
}

func parseIPv4(t *testing.T, s string) ([4]byte, bool) {
	t.Helper()
	var out [4]byte
	if s == "" {
		return out, false
	}
	parts := strings.Split(s, ".")
	if len(parts) != 4 {
		t.Fatalf("IPv4 形状不对: %q", s)
	}
	for i, p := range parts {
		v, err := strconv.Atoi(p)
		if err != nil {
			t.Fatalf("IPv4 段 %q 不是数字", p)
		}
		out[i] = byte(v)
	}
	return out, true
}

func parseIPv6(t *testing.T, s string) ([16]byte, bool) {
	t.Helper()
	var out [16]byte
	if s == "" {
		return out, false
	}
	groups := strings.Split(s, ":")
	if len(groups) != 8 {
		t.Fatalf("向量里的 IPv6 一律写成 8 组: %q", s)
	}
	for i, g := range groups {
		v, err := strconv.ParseUint(g, 16, 16)
		if err != nil {
			t.Fatalf("IPv6 段 %q 不合法", g)
		}
		out[i*2] = byte(v >> 8)
		out[i*2+1] = byte(v)
	}
	return out, true
}

func loadVectors(t *testing.T) []vector {
	t.Helper()
	raw, err := os.ReadFile(vectorsPath)
	if err != nil {
		t.Fatalf("读不到 %s: %v（先跑 cargo run --example gen_vectors）", vectorsPath, err)
	}

	var out []vector
	for _, line := range strings.Split(string(raw), "\n") {
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		f := strings.Split(line, "\t")
		if len(f) != 13 {
			t.Fatalf("向量列数不对（%d）: %q", len(f), line)
		}

		label, err := strconv.ParseUint(f[1], 10, 16)
		if err != nil {
			t.Fatalf("label 不合法: %q", f[1])
		}
		port, err := strconv.ParseUint(f[5], 10, 16)
		if err != nil {
			t.Fatalf("port 不合法: %q", f[5])
		}
		pinBytes, err := hex.DecodeString(f[6])
		if err != nil || len(pinBytes) != 8 {
			t.Fatalf("pin 不合法: %q", f[6])
		}
		var pin [8]byte
		copy(pin[:], pinBytes)

		ipv4, has4 := parseIPv4(t, f[2])
		ipv6, has6 := parseIPv6(t, f[3])

		out = append(out, vector{
			name: f[0], label: uint16(label),
			ipv4: ipv4, hasIPv4: has4,
			ipv6: ipv6, hasIPv6: has6,
			domain: f[4], port: uint16(port), pin: pin,
			relay:    f[7] == "true",
			bytesHex: f[8], code: f[9], room: f[10],
			fullInvite: f[11], shortInvite: f[12],
		})
	}
	if len(out) == 0 {
		t.Fatal("向量文件是空的")
	}
	return out
}

func (v vector) node() invite.NodeCode {
	return invite.NodeCode{
		Label:        v.label,
		IPv4:         v.ipv4,
		HasIPv4:      v.hasIPv4,
		IPv6:         v.ipv6,
		HasIPv6:      v.hasIPv6,
		Domain:       v.domain,
		Port:         v.port,
		Pin:          v.pin,
		RelayAllowed: v.relay,
	}
}

// 字节布局必须与 TSV 里的 bytes_hex 完全一致。
func TestBytesMatchVectors(t *testing.T) {
	for _, v := range loadVectors(t) {
		raw, err := v.node().Bytes()
		if err != nil {
			t.Fatalf("【%s】编码失败: %v", v.name, err)
		}
		got := strings.ToUpper(hex.EncodeToString(raw))
		if got != v.bytesHex {
			t.Fatalf("【%s】字节漂了\n实际 %s\n期望 %s", v.name, got, v.bytesHex)
		}
	}
}

// 节点码与 TSV 一致，且能原样解回来。
func TestCodeRoundTrips(t *testing.T) {
	for _, v := range loadVectors(t) {
		node := v.node()
		code, err := node.Encode()
		if err != nil {
			t.Fatalf("【%s】编码失败: %v", v.name, err)
		}
		if code != v.code {
			t.Fatalf("【%s】节点码漂了\n实际 %s\n期望 %s", v.name, code, v.code)
		}

		back, err := invite.Decode(code)
		if err != nil {
			t.Fatalf("【%s】解码失败: %v", v.name, err)
		}
		if back != node {
			t.Fatalf("【%s】解码结果和原节点不一致\n实际 %+v\n期望 %+v", v.name, back, node)
		}
	}
}

// 完整邀请 = 节点码 + "." + 门牌号。
func TestFullInviteRoundTrips(t *testing.T) {
	for _, v := range loadVectors(t) {
		node := v.node()
		full, err := invite.FullInvite(node, v.room)
		if err != nil {
			t.Fatalf("【%s】拼完整邀请失败: %v", v.name, err)
		}
		if full != v.fullInvite {
			t.Fatalf("【%s】完整邀请漂了\n实际 %s\n期望 %s", v.name, full, v.fullInvite)
		}

		parsed, err := invite.Parse(full)
		if err != nil {
			t.Fatalf("【%s】解析完整邀请失败: %v", v.name, err)
		}
		if parsed.Kind != invite.Full {
			t.Fatalf("【%s】Kind = %v，想要 Full", v.name, parsed.Kind)
		}
		if parsed.Room != v.room {
			t.Fatalf("【%s】门牌号 = %q，想要 %q", v.name, parsed.Room, v.room)
		}
		if parsed.Node == nil || *parsed.Node != node {
			t.Fatalf("【%s】完整邀请里的节点不一致", v.name)
		}
	}
}

// 短邀请 = 4 字符标签 + "." + 门牌号。
func TestShortInviteRoundTrips(t *testing.T) {
	for _, v := range loadVectors(t) {
		node := v.node()
		short := invite.ShortInvite(node, v.room)
		if short != v.shortInvite {
			t.Fatalf("【%s】短邀请漂了\n实际 %s\n期望 %s", v.name, short, v.shortInvite)
		}

		parsed, err := invite.Parse(short)
		if err != nil {
			t.Fatalf("【%s】解析短邀请失败: %v", v.name, err)
		}
		if parsed.Kind != invite.Short {
			t.Fatalf("【%s】Kind = %v，想要 Short", v.name, parsed.Kind)
		}
		if parsed.Label != node.LabelChars() || parsed.Room != v.room {
			t.Fatalf("【%s】短邀请解析结果 = %+v", v.name, parsed)
		}
	}
}

// 只粘门牌号、只粘节点码，两种都要认出来。
func TestBareRoomAndBareNode(t *testing.T) {
	v := loadVectors(t)[0]

	parsed, err := invite.Parse(v.room)
	if err != nil {
		t.Fatalf("解析门牌号失败: %v", err)
	}
	if parsed.Kind != invite.RoomOnly || parsed.Room != v.room {
		t.Fatalf("门牌号解析 = %+v", parsed)
	}

	parsed, err = invite.Parse(v.code)
	if err != nil {
		t.Fatalf("解析节点码失败: %v", err)
	}
	if parsed.Kind != invite.NodeOnly || parsed.Node == nil || *parsed.Node != v.node() {
		t.Fatalf("节点码解析 = %+v", parsed)
	}
}

// ————————————————————————————————————————————————
// 不依赖向量的锚点
// ————————————————————————————————————————————————

// CRC-8/SMBUS 的标准检查值，全世界都这么验。
func TestCRC8StandardCheckValue(t *testing.T) {
	if got := invite.CRC8([]byte("123456789")); got != 0xF4 {
		t.Fatalf("CRC8(\"123456789\") = %#02x，想要 0xF4", got)
	}
}

// pin 的定义：Ed25519 公钥的 SHA-256 前 8 字节。这里用标准库独立算一遍。
func TestPinFromPublicKey(t *testing.T) {
	pub, _, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatalf("生成密钥失败: %v", err)
	}

	pin := invite.PinFromPublicKey(pub)
	sum := sha256.Sum256(pub)
	if [8]byte(sum[:8]) != pin {
		t.Fatalf("pin = %x，想要 sha256 前 8 字节 %x", pin, sum[:8])
	}
	if invite.PinFromPublicKey(pub) != pin {
		t.Fatal("同一个公钥必须得到同一个 pin")
	}

	other, _, err := ed25519.GenerateKey(nil)
	if err != nil {
		t.Fatalf("生成密钥失败: %v", err)
	}
	if invite.PinFromPublicKey(other) == pin {
		t.Fatal("不同公钥不应得到同一个 pin")
	}
}

// 坏输入必须被拒绝，不许"尽力猜"。
func TestCorruptedInputIsRejected(t *testing.T) {
	v := loadVectors(t)[0]
	raw, err := hex.DecodeString(v.bytesHex)
	if err != nil {
		t.Fatal(err)
	}

	t.Run("CRC 被破坏", func(t *testing.T) {
		bad := append([]byte(nil), raw...)
		bad[len(bad)-1] ^= 0xff
		if _, err := invite.FromBytes(bad); !errors.Is(err, invite.ErrBadChecksum) {
			t.Fatalf("应当报 ErrBadChecksum，实际 %v", err)
		}
	})

	t.Run("截断", func(t *testing.T) {
		if _, err := invite.FromBytes(raw[:len(raw)-1]); err == nil {
			t.Fatal("截断必须报错")
		}
	})

	t.Run("多一字节", func(t *testing.T) {
		if _, err := invite.FromBytes(append(append([]byte(nil), raw...), 0)); err == nil {
			t.Fatal("多一字节必须报错")
		}
	})

	t.Run("版本号不认识", func(t *testing.T) {
		bad := append([]byte(nil), raw...)
		bad[0] = (2 << 4) | (bad[0] & 0x0f)
		bad[len(bad)-1] = invite.CRC8(bad[:len(bad)-1])
		if _, err := invite.FromBytes(bad); !errors.Is(err, invite.ErrBadVersion) {
			t.Fatalf("应当报 ErrBadVersion，实际 %v", err)
		}
	})
}

func TestInvalidNodeIsRejected(t *testing.T) {
	if _, err := (invite.NodeCode{Port: 443}).Bytes(); !errors.Is(err, invite.ErrNoAddress) {
		t.Fatalf("没有地址应当报 ErrNoAddress，实际 %v", err)
	}
	noPort := invite.NodeCode{HasIPv4: true, Port: 0}
	if _, err := noPort.Bytes(); !errors.Is(err, invite.ErrBadPort) {
		t.Fatalf("端口 0 应当报 ErrBadPort，实际 %v", err)
	}
	nonASCII := invite.NodeCode{Domain: "门牌.com", Port: 443}
	if _, err := nonASCII.Bytes(); !errors.Is(err, invite.ErrDomainASCII) {
		t.Fatalf("非 ASCII 域名应当报 ErrDomainASCII，实际 %v", err)
	}
}

func TestBrokenRoomCodeIsRejected(t *testing.T) {
	v := loadVectors(t)[0]

	if _, err := invite.Parse(v.code + ".A7K2M9X"); !errors.Is(err, invite.ErrBadRoomCode) {
		t.Fatalf("门牌号多一位应当报错，实际 %v", err)
	}
	if _, err := invite.Parse("   "); !errors.Is(err, invite.ErrEmpty) {
		t.Fatalf("空串应当报 ErrEmpty，实际 %v", err)
	}
}

func TestRoomCodeNormalisation(t *testing.T) {
	for _, input := range []string{"a7k2m9", " A7K2M9 ", "A7K2M9"} {
		parsed, err := invite.Parse(input)
		if err != nil {
			t.Fatalf("%q 解析失败: %v", input, err)
		}
		if parsed.Room != "A7K2M9" {
			t.Fatalf("%q 归一化成了 %q", input, parsed.Room)
		}
	}
}

func TestRoomCodeLength(t *testing.T) {
	for _, tc := range []struct {
		code string
		want bool
	}{
		{"A7K2M9", true},
		{"A7K2M", false},
		{"A7K2M9X", false},
		{"", false},
	} {
		if got := invite.IsRoomCode(tc.code); got != tc.want {
			t.Fatalf("IsRoomCode(%q) = %v，想要 %v", tc.code, got, tc.want)
		}
	}
}
