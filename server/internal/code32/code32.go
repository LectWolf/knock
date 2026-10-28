// Package code32 是 Crockford Base32：全项目**唯一**的字符表与编解码实现。
//
// 为什么单独一个包：门牌号和节点码都要用它，而 Rust 侧必须用完全一样的表。
// 两边漂一个字符，所有码就全废了，所以这张表只允许存在一份。
//
// Crockford 的三个规矩（也是它能被手抄的原因）：
//   - 字母表没有 I、L、O、U
//   - 解码时 O→0、I→1、L→1
//   - 大小写不敏感，连字符与空白被忽略
package code32

import (
	"errors"
	"fmt"
)

// Alphabet 是 Crockford Base32 的 32 个字符。
const Alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

var (
	ErrEmpty      = errors.New("code32: 空串")
	ErrBadChar    = errors.New("code32: 字母表外的字符")
	ErrBadLength  = errors.New("code32: 长度不整")
	ErrBadPadding = errors.New("code32: 尾部填充位不是 0")
)

// DecodeChar 把单个字符映射成 5 bit 值。
func DecodeChar(c byte) (byte, bool) {
	if c >= 'a' && c <= 'z' {
		c -= 'a' - 'A'
	}
	switch c {
	case 'O':
		c = '0'
	case 'I', 'L':
		c = '1'
	}
	for i := 0; i < len(Alphabet); i++ {
		if Alphabet[i] == c {
			return byte(i), true
		}
	}
	return 0, false
}

// IsSpaceLike 报告这个字节是不是解码时要跳过的分隔符。
func IsSpaceLike(c byte) bool {
	switch c {
	case '-', '_', ' ', '\t', '\r', '\n':
		return true
	}
	return false
}

// Encode 把字节串编成 Crockford Base32（尾部补 0 到整字符）。
func Encode(data []byte) string {
	out := make([]byte, 0, len(data)*8/5+2)
	var acc uint32
	var bits uint
	for _, b := range data {
		acc = (acc << 8) | uint32(b)
		bits += 8
		for bits >= 5 {
			bits -= 5
			out = append(out, Alphabet[(acc>>bits)&0x1f])
		}
	}
	if bits > 0 {
		out = append(out, Alphabet[(acc<<(5-bits))&0x1f])
	}
	return string(out)
}

// Decode 解回字节串。尾部多余的位必须为 0，否则视为串被截断或拼接过。
func Decode(s string) ([]byte, error) {
	out := make([]byte, 0, len(s)*5/8+1)
	var acc uint32
	var bits uint
	for i := 0; i < len(s); i++ {
		c := s[i]
		if IsSpaceLike(c) {
			continue
		}
		v, ok := DecodeChar(c)
		if !ok {
			return nil, fmt.Errorf("%w: %q", ErrBadChar, c)
		}
		acc = (acc << 5) | uint32(v)
		bits += 5
		if bits >= 8 {
			bits -= 8
			out = append(out, byte((acc>>bits)&0xff))
		}
	}
	if bits >= 5 {
		return nil, ErrBadLength
	}
	if bits > 0 && acc&((1<<bits)-1) != 0 {
		return nil, ErrBadPadding
	}
	if len(out) == 0 {
		return nil, ErrEmpty
	}
	return out, nil
}

// Normalize 去分隔符、转大写、把 O/I/L 换成 0/1。
func Normalize(s string) string {
	out := make([]byte, 0, len(s))
	for i := 0; i < len(s); i++ {
		c := s[i]
		if IsSpaceLike(c) {
			continue
		}
		if c >= 'a' && c <= 'z' {
			c -= 'a' - 'A'
		}
		switch c {
		case 'O':
			c = '0'
		case 'I', 'L':
			c = '1'
		}
		out = append(out, c)
	}
	return string(out)
}

// Valid 报告 s（已归一化）是不是全是字母表内的字符。
func Valid(s string) bool {
	if s == "" {
		return false
	}
	for i := 0; i < len(s); i++ {
		if _, ok := DecodeChar(s[i]); !ok {
			return false
		}
	}
	return true
}

// LabelChars 把 16 bit 标签写成 4 个字符：高位补 4 个 0 凑成 20 bit。
func LabelChars(v uint16) string {
	n := uint32(v)
	return string([]byte{
		Alphabet[(n>>15)&0x1f],
		Alphabet[(n>>10)&0x1f],
		Alphabet[(n>>5)&0x1f],
		Alphabet[n&0x1f],
	})
}

// LabelFromChars 把 4 个字符读回 16 bit；超出 16 bit 就报错。
func LabelFromChars(s string) (uint16, error) {
	var v uint32
	var count int
	for i := 0; i < len(s); i++ {
		c := s[i]
		if IsSpaceLike(c) {
			continue
		}
		d, ok := DecodeChar(c)
		if !ok {
			return 0, fmt.Errorf("%w: %q", ErrBadChar, c)
		}
		v = (v << 5) | uint32(d)
		count++
	}
	if count != 4 {
		return 0, ErrBadLength
	}
	if v > 0xffff {
		return 0, ErrBadLength
	}
	return uint16(v), nil
}
