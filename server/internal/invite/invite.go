// Package invite 是节点码与邀请串的编解码，与 Rust 侧 `knock::invite` 逐字节对齐。
//
// **格式的唯一真相是 `spec/invite-v1.tsv`**：Rust 生成、Go 读取，两边跑同一份向量。
// 本包改动必须同步改 Rust，否则 invite_test.go 会红。
//
// 布局 v1（多字节整数小端）：
//
//	+--------+---------+----------+-----------+-------+-----+-----+
//	|ver|flag|  label  | 地址们…  |   port    |  pin  | crc |
//	+--------+---------+----------+-----------+-------+-----+-----+
package invite

import (
	"crypto/ed25519"
	"crypto/sha256"
	"errors"
	"fmt"

	"knock/internal/code32"
)

const (
	Version    = 1
	PinLen     = 8
	RoomLen    = 6
	LabelChars = 4

	FlagIPv4   = 1 << 0
	FlagIPv6   = 1 << 1
	FlagDomain = 1 << 2
	FlagRelay  = 1 << 3

	MaxDomainLen = 253
)

var (
	ErrEmpty         = errors.New("invite: 空输入")
	ErrBadLength     = errors.New("invite: 字节长度不对")
	ErrBadVersion    = errors.New("invite: 版本号不认识")
	ErrNoAddress     = errors.New("invite: 一个地址都没有")
	ErrDomainEmpty   = errors.New("invite: 域名为空")
	ErrDomainTooLong = errors.New("invite: 域名太长")
	ErrDomainASCII   = errors.New("invite: 域名必须是 ASCII")
	ErrBadPort       = errors.New("invite: 端口不合法")
	ErrBadChecksum   = errors.New("invite: 校验和不匹配")
	ErrBadRoomCode   = errors.New("invite: 门牌号形状不对")
)

// NodeCode 是一个节点的自描述编码。**不含门牌号**——门牌号属于房间，不属于节点。
type NodeCode struct {
	Label        uint16
	IPv4         [4]byte
	HasIPv4      bool
	IPv6         [16]byte
	HasIPv6      bool
	Domain       string
	Port         uint16
	Pin          [PinLen]byte
	RelayAllowed bool
}

// CRC8 是 CRC-8/SMBUS：poly 0x07、init 0x00、不反射、xorout 0。
// 标准检查值 CRC8("123456789") == 0xF4，见测试。
func CRC8(data []byte) byte {
	var crc byte
	for _, b := range data {
		crc ^= b
		for i := 0; i < 8; i++ {
			if crc&0x80 != 0 {
				crc = (crc << 1) ^ 0x07
			} else {
				crc <<= 1
			}
		}
	}
	return crc
}

// Flags 把存在的地址与中转开关压成一个字节。
func (n NodeCode) Flags() byte {
	var f byte
	if n.HasIPv4 {
		f |= FlagIPv4
	}
	if n.HasIPv6 {
		f |= FlagIPv6
	}
	if n.Domain != "" {
		f |= FlagDomain
	}
	if n.RelayAllowed {
		f |= FlagRelay
	}
	return f
}

// Bytes 编成字节串（含末尾 CRC）。
func (n NodeCode) Bytes() ([]byte, error) {
	if !n.HasIPv4 && !n.HasIPv6 && n.Domain == "" {
		return nil, ErrNoAddress
	}
	if n.Port == 0 {
		return nil, fmt.Errorf("%w: 0", ErrBadPort)
	}

	out := make([]byte, 0, 40)
	out = append(out, (Version<<4)|n.Flags())
	out = append(out, byte(n.Label), byte(n.Label>>8))
	if n.HasIPv4 {
		out = append(out, n.IPv4[:]...)
	}
	if n.HasIPv6 {
		out = append(out, n.IPv6[:]...)
	}
	if n.Domain != "" {
		if len(n.Domain) > MaxDomainLen {
			return nil, ErrDomainTooLong
		}
		for i := 0; i < len(n.Domain); i++ {
			if n.Domain[i] >= 0x80 {
				return nil, ErrDomainASCII
			}
		}
		out = append(out, byte(len(n.Domain)))
		out = append(out, n.Domain...)
	}
	out = append(out, byte(n.Port), byte(n.Port>>8))
	out = append(out, n.Pin[:]...)
	out = append(out, CRC8(out))
	return out, nil
}

// FromBytes 从字节串还原。任何一字节不对都报错，不"尽力猜"。
func FromBytes(data []byte) (NodeCode, error) {
	var n NodeCode
	if len(data) < 4 {
		return n, ErrBadLength
	}

	version := data[0] >> 4
	if version != Version {
		return n, fmt.Errorf("%w: %d", ErrBadVersion, version)
	}
	flags := data[0] & 0x0f
	if flags&(FlagIPv4|FlagIPv6|FlagDomain) == 0 {
		return n, ErrNoAddress
	}

	pos := 1
	take := func(size int) ([]byte, error) {
		if pos+size > len(data) {
			return nil, ErrBadLength
		}
		slice := data[pos : pos+size]
		pos += size
		return slice, nil
	}

	labelBytes, err := take(2)
	if err != nil {
		return n, err
	}
	n.Label = uint16(labelBytes[0]) | uint16(labelBytes[1])<<8

	if flags&FlagIPv4 != 0 {
		raw, err := take(4)
		if err != nil {
			return n, err
		}
		copy(n.IPv4[:], raw)
		n.HasIPv4 = true
	}

	if flags&FlagIPv6 != 0 {
		raw, err := take(16)
		if err != nil {
			return n, err
		}
		copy(n.IPv6[:], raw)
		n.HasIPv6 = true
	}

	if flags&FlagDomain != 0 {
		lenByte, err := take(1)
		if err != nil {
			return n, err
		}
		size := int(lenByte[0])
		if size == 0 {
			return n, ErrDomainEmpty
		}
		raw, err := take(size)
		if err != nil {
			return n, err
		}
		for _, b := range raw {
			if b >= 0x80 {
				return n, ErrDomainASCII
			}
		}
		n.Domain = string(raw)
	}

	portBytes, err := take(2)
	if err != nil {
		return n, err
	}
	n.Port = uint16(portBytes[0]) | uint16(portBytes[1])<<8
	if n.Port == 0 {
		return n, fmt.Errorf("%w: 0", ErrBadPort)
	}

	pinBytes, err := take(PinLen)
	if err != nil {
		return n, err
	}
	copy(n.Pin[:], pinBytes)

	wantBytes, err := take(1)
	if err != nil {
		return n, err
	}
	want := wantBytes[0]
	got := CRC8(data[:pos-1])
	if want != got {
		return n, fmt.Errorf("%w: 期望 %#02x 实际 %#02x", ErrBadChecksum, want, got)
	}
	if pos != len(data) {
		return n, ErrBadLength
	}

	n.RelayAllowed = flags&FlagRelay != 0
	return n, nil
}

// Encode 得到规范形式：Crockford Base32，大写、无分隔符。
func (n NodeCode) Encode() (string, error) {
	raw, err := n.Bytes()
	if err != nil {
		return "", err
	}
	return code32.Encode(raw), nil
}

// Decode 解析规范形式（也接受带连字符、小写、O/I/L 别名的输入）。
func Decode(s string) (NodeCode, error) {
	raw, err := code32.Decode(s)
	if err != nil {
		return NodeCode{}, err
	}
	return FromBytes(raw)
}

// LabelChars 是短邀请里那 4 个字符。
func (n NodeCode) LabelChars() string { return code32.LabelChars(n.Label) }

// Kind 是邀请串的四种形态。
type Kind int

const (
	RoomOnly Kind = iota
	Short
	Full
	NodeOnly
)

// Invite 是用户粘进来的东西解析之后的结果。
type Invite struct {
	Kind  Kind
	Room  string
	Label string
	Node  *NodeCode
}

// Parse 解析用户粘贴的任意一串东西。
func Parse(input string) (Invite, error) {
	cleaned := sanitize(input)
	if cleaned == "" {
		return Invite{}, ErrEmpty
	}

	dot := -1
	for i := 0; i < len(cleaned); i++ {
		if cleaned[i] == '.' {
			dot = i
			break
		}
	}

	if dot < 0 {
		// 没有分隔符：要么是光秃秃的门牌号，要么是光秃秃的节点码
		if IsRoomCode(cleaned) {
			return Invite{Kind: RoomOnly, Room: code32.Normalize(cleaned)}, nil
		}
		node, err := Decode(cleaned)
		if err != nil {
			return Invite{}, err
		}
		return Invite{Kind: NodeOnly, Node: &node}, nil
	}

	left, right := cleaned[:dot], cleaned[dot+1:]
	if !IsRoomCode(right) {
		return Invite{}, ErrBadRoomCode
	}
	room := code32.Normalize(right)

	// 左边 4 个字符只可能是标签；节点码最短 29 个字符，不会撞
	if len(code32.Normalize(left)) == LabelChars {
		return Invite{Kind: Short, Label: code32.Normalize(left), Room: room}, nil
	}

	node, err := Decode(left)
	if err != nil {
		return Invite{}, err
	}
	return Invite{Kind: Full, Node: &node, Room: room}, nil
}

// FullInvite 拼出完整邀请：`<节点码>.<门牌号>`。
func FullInvite(node NodeCode, room string) (string, error) {
	code, err := node.Encode()
	if err != nil {
		return "", err
	}
	return code + "." + code32.Normalize(room), nil
}

// ShortInvite 拼出短邀请：`<标签>.<门牌号>`。
func ShortInvite(node NodeCode, room string) string {
	return node.LabelChars() + "." + code32.Normalize(room)
}

// IsRoomCode 报告是不是 6 个合法字符（大小写与别名都接受）。
func IsRoomCode(raw string) bool {
	n := code32.Normalize(raw)
	return len(n) == RoomLen && code32.Valid(n)
}

// PinFromPublicKey 是节点码里 `pin` 字段的来源：Ed25519 公钥的 SHA-256 前 8 字节。
//
// 8 字节（64 bit）是刻意的折中：伪造需要 2^64 次哈希，同时比放整把公钥省下 22 个字符。
// Rust 侧将来校验握手时要用同样的算法（届时随 iroh 一起引入哈希实现）。
func PinFromPublicKey(pub ed25519.PublicKey) [PinLen]byte {
	sum := sha256.Sum256(pub)
	var pin [PinLen]byte
	copy(pin[:], sum[:PinLen])
	return pin
}

func sanitize(s string) string {
	out := make([]byte, 0, len(s))
	for i := 0; i < len(s); i++ {
		switch s[i] {
		case ' ', '\t', '\r', '\n':
			continue
		}
		out = append(out, s[i])
	}
	return string(out)
}
