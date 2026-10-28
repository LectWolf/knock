package node_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"knock/internal/invite"
	"knock/internal/node"
)

func pin() [8]byte { return [8]byte{0x01, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88} }

func TestDefaultsAreUsable(t *testing.T) {
	cfg := node.DefaultConfig()
	if cfg.PublicPort == 0 {
		t.Fatal("默认端口不该是 0")
	}
	if cfg.RoomTTL == "" {
		t.Fatal("默认 TTL 不该为空")
	}
	// 默认没有地址，所以默认配置本身是"待填写"状态——这是刻意的
	if err := cfg.Validate(); err == nil {
		t.Fatal("没有地址的默认配置应当校验失败，逼主人填一个")
	}
}

func TestLoadMissingConfigReturnsDefaults(t *testing.T) {
	cfg, err := node.LoadConfig(filepath.Join(t.TempDir(), "nope.json"))
	if err != nil {
		t.Fatalf("文件不存在不该报错: %v", err)
	}
	if cfg.PublicPort != node.DefaultConfig().PublicPort {
		t.Fatalf("应当是默认配置，实际 %+v", cfg)
	}
}

func TestWriteThenLoadRoundTrips(t *testing.T) {
	path := filepath.Join(t.TempDir(), node.ConfigFileName)

	want := node.DefaultConfig()
	want.Domain = "relay.example.com"
	want.IPv4 = "203.0.113.7"
	want.Label = "04HM"
	want.MaxRooms = 42

	if err := node.WriteConfig(path, want); err != nil {
		t.Fatalf("写配置失败: %v", err)
	}
	got, err := node.LoadConfig(path)
	if err != nil {
		t.Fatalf("读配置失败: %v", err)
	}
	if got != want {
		t.Fatalf("配置往返不一致\n实际 %+v\n期望 %+v", got, want)
	}
}

func TestWriteConfigDoesNotOverwrite(t *testing.T) {
	path := filepath.Join(t.TempDir(), node.ConfigFileName)
	if err := node.WriteConfig(path, node.DefaultConfig()); err != nil {
		t.Fatal(err)
	}
	if err := node.WriteConfig(path, node.DefaultConfig()); err == nil {
		t.Fatal("已存在的配置不该被覆盖")
	}
}

func TestValidateRejectsBrokenConfigs(t *testing.T) {
	base := node.DefaultConfig()
	base.IPv4 = "203.0.113.7"

	cases := map[string]func(*node.Config){
		"没有地址":       func(c *node.Config) { c.IPv4 = "" },
		"端口为 0":      func(c *node.Config) { c.PublicPort = 0 },
		"监听为空":       func(c *node.Config) { c.Listen = "" },
		"房间上限为负":     func(c *node.Config) { c.MaxRooms = -1 },
		"TTL 不是时长":   func(c *node.Config) { c.RoomTTL = "十分钟" },
		"TTL 为 0":    func(c *node.Config) { c.RoomTTL = "0s" },
		"标签长度不对":     func(c *node.Config) { c.Label = "ABC" },
		"标签有非法字符":    func(c *node.Config) { c.Label = "AB!C" },
		"IPv4 不合法":   func(c *node.Config) { c.IPv4 = "999.1.1.1" },
		"IPv6 填了 v4": func(c *node.Config) { c.IPv6 = "1.2.3.4" },
		// 公网明文 HTTP 会被中间设备拦截甚至替换，必须拒绝
		"中继用明文 http": func(c *node.Config) { c.RelayURL = "http://relay.example.com" },
		"中继地址不是地址":   func(c *node.Config) { c.RelayURL = "relay.example.com" },
		"中继协议不认识":    func(c *node.Config) { c.RelayURL = "quic://relay.example.com" },
		// 名字会显示在客户端上，太长或带控制字符都要挡住
		"名字太长":   func(c *node.Config) { c.Name = strings.Repeat("香", node.NameMaxChars+1) },
		"名字有控制符": func(c *node.Config) { c.Name = "香港" + string(rune(7)) + "荃湾" },
	}

	for name, mutate := range cases {
		cfg := base
		mutate(&cfg)
		if err := cfg.Validate(); err == nil {
			t.Fatalf("%s：应当校验失败，实际通过", name)
		}
	}

	if err := base.Validate(); err != nil {
		t.Fatalf("合法配置不该报错: %v", err)
	}
}

func TestNodeCodeRoundTripsThroughEncoding(t *testing.T) {
	cfg := node.DefaultConfig()
	cfg.Domain = "relay.example.com"
	cfg.IPv4 = "203.0.113.7"
	cfg.IPv6 = "2001:db8::1"
	cfg.PublicPort = 8443
	cfg.RelayAllowed = true

	code, err := cfg.NodeCode(pin())
	if err != nil {
		t.Fatalf("拼节点码失败: %v", err)
	}
	text, err := code.Encode()
	if err != nil {
		t.Fatalf("编码失败: %v", err)
	}

	back, err := invite.Decode(text)
	if err != nil {
		t.Fatalf("解码失败: %v", err)
	}
	if back != code {
		t.Fatalf("往返不一致\n实际 %+v\n期望 %+v", back, code)
	}
}

func TestLabelDerivedFromPinIsStable(t *testing.T) {
	cfg := node.DefaultConfig()
	cfg.IPv4 = "203.0.113.7"

	first, err := cfg.LabelValue(pin())
	if err != nil {
		t.Fatal(err)
	}
	second, err := cfg.LabelValue(pin())
	if err != nil {
		t.Fatal(err)
	}
	if first != second {
		t.Fatal("同一个 pin 必须得到同一个标签")
	}

	other := [8]byte{0xAA, 0xBB}
	if v, _ := cfg.LabelValue(other); v == first {
		t.Fatal("不同 pin 应当得到不同标签（绝大多数情况下）")
	}

	cfg.Label = "04HM"
	explicit, err := cfg.LabelValue(pin())
	if err != nil {
		t.Fatalf("显式标签应当合法: %v", err)
	}
	if explicit != 0x1234 {
		// 金标向量里 label=0x1234 的短邀请正是 04HM
		t.Fatalf("04HM 解出来是 %#x，想要 0x1234", explicit)
	}
}

func TestReachabilityWarning(t *testing.T) {
	cases := []struct {
		name     string
		cfg      node.Config
		wantWarn bool
	}{
		{"公网 IPv4", node.Config{IPv4: "203.0.113.7"}, false}, // 203.0.113.x 是文档段但按公网算
		{"私网 IPv4", node.Config{IPv4: "192.168.1.10"}, true},
		{"环回", node.Config{IPv4: "127.0.0.1"}, true},
		{"运营商 NAT", node.Config{IPv4: "100.72.1.1"}, true},
		{"公网 IPv6", node.Config{IPv6: "2001:db8::1"}, false}, // 文档段，按公网算
		{"链路本地 IPv6", node.Config{IPv6: "fe80::1"}, true},
		{"有域名就不警告", node.Config{Domain: "relay.example.com", IPv4: "192.168.1.10"}, false},
	}

	for _, tc := range cases {
		got := tc.cfg.ReachabilityWarning()
		if tc.wantWarn && got == "" {
			t.Fatalf("%s：应当给出可达性警告", tc.name)
		}
		if !tc.wantWarn && got != "" {
			t.Fatalf("%s：不该警告，实际：%s", tc.name, got)
		}
	}
}

func TestRelayURLAcceptsHTTPSAndLocalHTTP(t *testing.T) {
	base := node.DefaultConfig()
	base.Domain = "relay.example.com"

	for _, ok := range []string{
		"",
		"https://relay.example.com",
		"https://relay.example.com:8443",
		"http://127.0.0.1:8080", // 本机调试允许明文
		"http://localhost:8080",
	} {
		cfg := base
		cfg.RelayURL = ok
		if err := cfg.Validate(); err != nil {
			t.Fatalf("%q 应当合法: %v", ok, err)
		}
	}
}

func TestTTLParsing(t *testing.T) {
	cfg := node.DefaultConfig()
	cfg.RoomTTL = "90s"
	ttl, err := cfg.TTL()
	if err != nil {
		t.Fatal(err)
	}
	if ttl != 90*time.Second {
		t.Fatalf("TTL = %s，想要 90s", ttl)
	}
}

func TestBannerShowsTheThingsPeopleCopy(t *testing.T) {
	cfg := node.DefaultConfig()
	cfg.Domain = "relay.example.com"
	cfg.PublicPort = 8443

	code, err := cfg.NodeCode(pin())
	if err != nil {
		t.Fatal(err)
	}
	text, err := code.Encode()
	if err != nil {
		t.Fatal(err)
	}

	banner := node.Banner(cfg, code, text)

	for _, must := range []string{text, code.LabelChars(), "8443", "relay.example.com", "数据中转"} {
		if !strings.Contains(banner, must) {
			t.Fatalf("启动横幅里缺少 %q\n%s", must, banner)
		}
	}
}

func TestBannerSurfacesReachabilityWarning(t *testing.T) {
	cfg := node.DefaultConfig()
	cfg.IPv4 = "192.168.1.10"

	code, err := cfg.NodeCode(pin())
	if err != nil {
		t.Fatal(err)
	}
	banner := node.Banner(cfg, code, "X")

	if !strings.Contains(banner, "⚠") {
		t.Fatalf("私网地址应当在横幅里警告\n%s", banner)
	}
}

func TestWriteConfigCreatesFile(t *testing.T) {
	dir := t.TempDir()
	nested := filepath.Join(dir, "sub", node.ConfigFileName)
	if err := node.WriteConfig(nested, node.DefaultConfig()); err != nil {
		t.Fatalf("应当自动创建父目录: %v", err)
	}
	if _, err := os.Stat(nested); err != nil {
		t.Fatalf("配置文件没写出来: %v", err)
	}
}
