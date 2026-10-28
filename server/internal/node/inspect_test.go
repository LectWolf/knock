package node_test

import (
	"strings"
	"testing"

	"knock/internal/invite"
	"knock/internal/node"
)

func sampleNode(t *testing.T) invite.NodeCode {
	t.Helper()
	cfg := node.DefaultConfig()
	cfg.Domain = "relay.example.com"
	cfg.PublicPort = 8443
	code, err := cfg.NodeCode(pin())
	if err != nil {
		t.Fatalf("拼节点码失败: %v", err)
	}
	return code
}

func TestInspectFullInvite(t *testing.T) {
	code := sampleNode(t)
	text, err := code.Encode()
	if err != nil {
		t.Fatal(err)
	}

	out, err := node.Inspect(text + ".A7K2M9")
	if err != nil {
		t.Fatalf("解析完整邀请失败: %v", err)
	}

	for _, must := range []string{"完整邀请", code.LabelChars(), "A7K2M9", "8443", "relay.example.com"} {
		if !strings.Contains(out, must) {
			t.Fatalf("输出里缺少 %q\n%s", must, out)
		}
	}
}

func TestInspectNodeOnly(t *testing.T) {
	code := sampleNode(t)
	text, _ := code.Encode()

	out, err := node.Inspect(text)
	if err != nil {
		t.Fatalf("解析节点码失败: %v", err)
	}
	if !strings.Contains(out, "节点码") || !strings.Contains(out, code.LabelChars()) {
		t.Fatalf("应当说明这是节点码并给出标签\n%s", out)
	}
	if strings.Contains(out, "门牌号：") {
		t.Fatalf("节点码里没有门牌号，不该打印出来\n%s", out)
	}
}

func TestInspectRoomOnly(t *testing.T) {
	out, err := node.Inspect("A7K2M9")
	if err != nil {
		t.Fatalf("解析门牌号失败: %v", err)
	}
	if !strings.Contains(out, "门牌号") {
		t.Fatalf("没能识别出门牌号\n%s", out)
	}
	// 这是最容易犯的错：只发了个门牌号就以为对方能敲
	if !strings.Contains(out, "节点") {
		t.Fatalf("应当提醒「门牌号离不开节点」\n%s", out)
	}
}

func TestInspectShortInvite(t *testing.T) {
	out, err := node.Inspect("04HM.A7K2M9")
	if err != nil {
		t.Fatalf("解析短邀请失败: %v", err)
	}
	if !strings.Contains(out, "短邀请") || !strings.Contains(out, "04HM") {
		t.Fatalf("应当说明这是短邀请\n%s", out)
	}
}

func TestInspectRejectsGarbage(t *testing.T) {
	if _, err := node.Inspect("这不是码"); err == nil {
		t.Fatal("垃圾输入应当报错")
	}
	if _, err := node.Inspect(""); err == nil {
		t.Fatal("空串应当报错")
	}
}

// 输错的码必须被拒绝，而不是"接近就好"——CRC 就是干这个的。
func TestInspectRejectsCorruptedCode(t *testing.T) {
	code := sampleNode(t)
	text, _ := code.Encode()

	// 把最后一个字符换掉：CRC 一定对不上
	last := text[len(text)-1]
	var replacement byte = '0'
	if last == '0' {
		replacement = '1'
	}
	corrupted := text[:len(text)-1] + string(replacement)

	if _, err := node.Inspect(corrupted + ".A7K2M9"); err == nil {
		t.Fatal("改坏一个字符的码应当被拒绝")
	}
}
