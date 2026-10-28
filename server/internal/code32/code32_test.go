package code32_test

import (
	"errors"
	"testing"

	"knock/internal/code32"
)

// 手算：0x01 0x02 的比特流 00000 00100 00001 (补 0) → "0410"。
func TestHandCheckedEncode(t *testing.T) {
	if got := code32.Encode([]byte{0x01, 0x02}); got != "0410" {
		t.Fatalf("Encode = %q，想要 0410", got)
	}
	raw, err := code32.Decode("0410")
	if err != nil {
		t.Fatalf("Decode 失败: %v", err)
	}
	if len(raw) != 2 || raw[0] != 0x01 || raw[1] != 0x02 {
		t.Fatalf("Decode = %v，想要 [1 2]", raw)
	}
}

// 字母表必须恰好是这 32 个字符，且没有 I/L/O/U。
func TestAlphabetIsLocked(t *testing.T) {
	const want = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
	if code32.Alphabet != want {
		t.Fatalf("字母表 = %q，想要 %q", code32.Alphabet, want)
	}
	for _, forbidden := range []byte("ILOU") {
		for i := 0; i < len(code32.Alphabet); i++ {
			if code32.Alphabet[i] == forbidden {
				t.Fatalf("字母表里不该有 %q", forbidden)
			}
		}
	}
}

// Crockford 别名：O→0、I/L→1、小写照收。
func TestCrockfordAliases(t *testing.T) {
	canonical, err := code32.Decode("0410")
	if err != nil {
		t.Fatalf("基准串解不开: %v", err)
	}
	for _, alias := range []string{"O41O", "04I0", "04L0", "o4i0"} {
		got, err := code32.Decode(alias)
		if err != nil {
			t.Fatalf("%q 应当能解: %v", alias, err)
		}
		if string(got) != string(canonical) {
			t.Fatalf("%q 解成了 %v，想要 %v", alias, got, canonical)
		}
	}
}

// 显示用的连字符、空格不影响解码。
func TestSeparatorsAreIgnored(t *testing.T) {
	a, err := code32.Decode("0410")
	if err != nil {
		t.Fatal(err)
	}
	b, err := code32.Decode(" 0-41 0 ")
	if err != nil {
		t.Fatalf("带分隔符也要能解: %v", err)
	}
	if string(a) != string(b) {
		t.Fatalf("%v != %v", a, b)
	}
}

// 尾部填充位不是 0，说明串被截断或拼接过，必须报错。
func TestNonZeroPaddingIsRejected(t *testing.T) {
	_, err := code32.Decode("0411")
	if !errors.Is(err, code32.ErrBadPadding) {
		t.Fatalf("应当报 ErrBadPadding，实际 %v", err)
	}
}

func TestBadCharIsRejected(t *testing.T) {
	_, err := code32.Decode("A7K2M!")
	if !errors.Is(err, code32.ErrBadChar) {
		t.Fatalf("应当报 ErrBadChar，实际 %v", err)
	}
}

func TestLabelChars(t *testing.T) {
	if got := code32.LabelChars(0); got != "0000" {
		t.Fatalf("LabelChars(0) = %q", got)
	}
	if got := code32.LabelChars(0xFFFF); got != "1ZZZ" {
		t.Fatalf("LabelChars(0xFFFF) = %q，想要 1ZZZ", got)
	}
}

// 标签只有 16 bit：4 个字符拼出超过 0xFFFF 就是错码，不许截断。
func TestLabelOverflowIsRejected(t *testing.T) {
	if _, err := code32.LabelFromChars("ZZZZ"); err == nil {
		t.Fatal("ZZZZ 超出了 16 bit，应当报错")
	}
	v, err := code32.LabelFromChars("1ZZZ")
	if err != nil {
		t.Fatalf("1ZZZ 应当合法: %v", err)
	}
	if v != 0xFFFF {
		t.Fatalf("1ZZZ = %#x，想要 0xffff", v)
	}
}

// 任意字节都要能原样往返（含长度互补的所有余数情况）。
func TestRoundTripAllRemainders(t *testing.T) {
	for n := 1; n <= 40; n++ {
		data := make([]byte, n)
		for i := range data {
			data[i] = byte((i*37 + n*11) & 0xff)
		}
		encoded := code32.Encode(data)
		back, err := code32.Decode(encoded)
		if err != nil {
			t.Fatalf("%d 字节解码失败: %v", n, err)
		}
		if string(back) != string(data) {
			t.Fatalf("%d 字节往返不一致", n)
		}
	}
}
