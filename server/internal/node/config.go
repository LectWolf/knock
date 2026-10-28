package node

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"

	"knock/internal/code32"
	"knock/internal/invite"
)

// ConfigFileName 是配置文件在数据目录里的默认名字。
const ConfigFileName = "node.json"

// Config 是节点主人能在配置文件里改的全部东西。
//
// 刻意做成"一屏能看完"的扁平结构：节点是给人自托管的，配置项多了没人调得动。
type Config struct {
	// Label 是节点自选标签（4 个 Crockford 字符），用于短邀请。
	// 留空则由 pin 派生，保证同一台机器的标签稳定。
	Label string `json:"label"`

	// PublicPort 是别人连这个节点用的端口，会写进节点码。
	PublicPort uint16 `json:"public_port"`

	// 三个地址至少填一个。域名优先（IP 变了不用重发码）。
	Domain string `json:"domain"`
	IPv4   string `json:"ipv4"`
	IPv6   string `json:"ipv6"`

	// RelayAllowed 决定这个节点是否替别人转发流量。
	// 关掉仍然能当信令点用（只是打洞失败就没退路了）。
	RelayAllowed bool `json:"relay_allowed"`

	// RelayURL 是这个节点自己的中继地址（iroh-relay），形如 https://relay.example.com。
	//
	// 留空 = 这个节点只做信令，打洞失败就没有退路。
	// 有值 = 客户端会把它作为**唯一**的中继，不会回落到别人的公共服务。
	RelayURL string `json:"relay_url"`

	// Listen 是 HTTP API 的监听地址。
	Listen string `json:"listen"`

	// RoomTTL 是房间寿命，Go duration 字符串（如 "10m"）。
	RoomTTL string `json:"room_ttl"`

	// MaxRooms 是同时开着的房间上限，0 表示不限制。
	MaxRooms int `json:"max_rooms"`

	// LookupPerMinute 是同一个 IP 每分钟能查多少次门牌号。
	// 门牌号只有 6 位，不限速就等于允许别人把整个空间扫一遍。
	// 0 表示不限制——**公网节点不要这么配**。
	LookupPerMinute int `json:"lookup_per_minute"`

	// CreatePerMinute 是同一个 IP 每分钟能开多少次门。0 表示不限制。
	CreatePerMinute int `json:"create_per_minute"`
}

// DefaultConfig 是首次启动时写进配置文件的内容。
func DefaultConfig() Config {
	return Config{
		Label:           "",
		PublicPort:      8443,
		RelayAllowed:    true,
		RelayURL:        "",
		Listen:          ":8443",
		RoomTTL:         "10m",
		MaxRooms:        200,
		LookupPerMinute: 30,
		CreatePerMinute: 5,
	}
}

// LoadConfig 读配置文件。文件不存在时返回默认值且不报错（首次启动就是这个路径）。
func LoadConfig(path string) (Config, error) {
	cfg := DefaultConfig()

	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return cfg, nil
	}
	if err != nil {
		return cfg, fmt.Errorf("读配置 %s: %w", path, err)
	}

	// 以默认值为底再覆盖，这样配置里只写想改的字段就够了
	if err := json.Unmarshal(data, &cfg); err != nil {
		return cfg, fmt.Errorf("解析配置 %s: %w", path, err)
	}
	return cfg, nil
}

// WriteConfig 把配置写到磁盘（已存在则不覆盖，交由人决定）。
func WriteConfig(path string, cfg Config) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	data, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return err
	}
	data = append(data, '\n')

	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o644)
	if err != nil {
		return err
	}
	defer f.Close()
	_, err = f.Write(data)
	return err
}

// validateRelayURL 要求中继地址必须是 https（本机调试除外）。
//
// 理由不是"加密流量"——中继本来就只转发密文，看不到内容。
// 理由是**明文 HTTP 在链路上会被中间设备改写**：运营商、酒店门户、公共 Wi-Fi
// 都可能把 http 响应换成自己的页面或注入脚本。地址被换掉，客户端就会去连一台错的机器，
// 虽然连上去也解不开密文，但会白折腾一轮、而且看起来像"门打不开"。
func (c Config) validateRelayURL() error {
	if c.RelayURL == "" {
		return nil
	}
	u, err := url.Parse(c.RelayURL)
	if err != nil || u.Host == "" {
		return fmt.Errorf("relay_url %q 不是合法地址（形如 https://relay.example.com）", c.RelayURL)
	}
	switch u.Scheme {
	case "https":
		return nil
	case "http":
		if isLoopbackHost(u.Hostname()) {
			return nil // 本机调试允许明文
		}
		return errors.New("relay_url 必须用 https：公网上的明文 HTTP 会被中间设备拦截甚至替换")
	default:
		return fmt.Errorf("relay_url 的协议 %q 不认识，只支持 https（本机可 http）", u.Scheme)
	}
}

func isLoopbackHost(host string) bool {
	if host == "localhost" {
		return true
	}
	ip, err := netip.ParseAddr(host)
	return err == nil && ip.IsLoopback()
}

// TTL 解析房间寿命。
func (c Config) TTL() (time.Duration, error) {
	ttl, err := time.ParseDuration(c.RoomTTL)
	if err != nil {
		return 0, fmt.Errorf("room_ttl %q 不合法: %w", c.RoomTTL, err)
	}
	if ttl <= 0 {
		return 0, fmt.Errorf("room_ttl 必须大于 0，收到 %s", ttl)
	}
	return ttl, nil
}

// Validate 在启动前把配置错误一次性说清楚，而不是等第一个请求来才炸。
func (c Config) Validate() error {
	if c.PublicPort == 0 {
		return errors.New("public_port 不能是 0")
	}
	if c.Domain == "" && c.IPv4 == "" && c.IPv6 == "" {
		return errors.New("至少要填一个地址：domain、ipv4 或 ipv6")
	}
	if c.Listen == "" {
		return errors.New("listen 不能为空")
	}
	if c.MaxRooms < 0 {
		return errors.New("max_rooms 不能是负数")
	}
	if c.LookupPerMinute < 0 {
		return errors.New("lookup_per_minute 不能是负数")
	}
	if c.CreatePerMinute < 0 {
		return errors.New("create_per_minute 不能是负数")
	}
	if _, err := c.TTL(); err != nil {
		return err
	}
	if c.Label != "" {
		if _, err := code32.LabelFromChars(c.Label); err != nil {
			return fmt.Errorf("label %q 必须是 4 个 Crockford 字符: %w", c.Label, err)
		}
	}
	if err := c.validateRelayURL(); err != nil {
		return err
	}
	if _, err := c.parseIPv4(); err != nil {
		return err
	}
	if _, err := c.parseIPv6(); err != nil {
		return err
	}
	return nil
}

func (c Config) parseIPv4() ([4]byte, error) {
	var out [4]byte
	if c.IPv4 == "" {
		return out, nil
	}
	addr, err := netip.ParseAddr(c.IPv4)
	if err != nil || !addr.Is4() {
		return out, fmt.Errorf("ipv4 %q 不合法", c.IPv4)
	}
	return addr.As4(), nil
}

func (c Config) parseIPv6() ([16]byte, error) {
	var out [16]byte
	if c.IPv6 == "" {
		return out, nil
	}
	addr, err := netip.ParseAddr(c.IPv6)
	if err != nil || !addr.Is6() || addr.Is4In6() {
		return out, fmt.Errorf("ipv6 %q 不合法", c.IPv6)
	}
	return addr.As16(), nil
}

// LabelValue 给出写进节点码的 16 bit 标签：配置里写了就用它，否则由 pin 派生。
//
// 派生是刻意"稳定"的——同一台机器只要密钥还在，短邀请就还能用。
func (c Config) LabelValue(pin [invite.PinLen]byte) (uint16, error) {
	if c.Label != "" {
		return code32.LabelFromChars(c.Label)
	}
	return uint16(pin[0]) | uint16(pin[1])<<8, nil
}

// NodeCode 把配置 + 身份拼成给对方粘贴的那串东西。
func (c Config) NodeCode(pin [invite.PinLen]byte) (invite.NodeCode, error) {
	if err := c.Validate(); err != nil {
		return invite.NodeCode{}, err
	}

	label, err := c.LabelValue(pin)
	if err != nil {
		return invite.NodeCode{}, err
	}

	node := invite.NodeCode{
		Label:        label,
		Domain:       strings.ToLower(c.Domain),
		Port:         c.PublicPort,
		Pin:          pin,
		RelayAllowed: c.RelayAllowed,
	}

	if v4, err := c.parseIPv4(); err != nil {
		return invite.NodeCode{}, err
	} else if c.IPv4 != "" {
		node.IPv4 = v4
		node.HasIPv4 = true
	}

	if v6, err := c.parseIPv6(); err != nil {
		return invite.NodeCode{}, err
	} else if c.IPv6 != "" {
		node.IPv6 = v6
		node.HasIPv6 = true
	}

	return node, nil
}

// ReachabilityWarning 在配置里的地址明显不可达时给一句人话。
//
// 这是 ADR-0002 那条代价的落地：绝大多数家用网络根本当中继不了，
// 与其让人折腾半天，不如启动时就说清楚。
func (c Config) ReachabilityWarning() string {
	if c.Domain != "" {
		return "" // 有域名就认为主人自己解决了可达性
	}

	var checked []string
	reachable := false
	for _, raw := range []string{c.IPv4, c.IPv6} {
		if raw == "" {
			continue
		}
		addr, err := netip.ParseAddr(raw)
		if err != nil {
			continue
		}
		checked = append(checked, raw)
		if isPublic(addr) {
			reachable = true
		}
	}
	if reachable || len(checked) == 0 {
		return ""
	}
	return fmt.Sprintf(
		"填的地址（%s）不是公网可达地址：别人大概率连不上这个节点。\n"+
			"  要么换一台有公网 IP 的机器，要么在路由器做端口映射，要么填一个指向它的域名。",
		strings.Join(checked, ", "),
	)
}

// isPublic 判断一个地址是不是"公网上别人能直接连到"的。
func isPublic(addr netip.Addr) bool {
	if !addr.IsValid() || addr.IsLoopback() || addr.IsPrivate() ||
		addr.IsLinkLocalUnicast() || addr.IsLinkLocalMulticast() ||
		addr.IsMulticast() || addr.IsUnspecified() {
		return false
	}
	if addr.Is4() {
		b := addr.As4()
		// 运营商级 NAT（100.64.0.0/10）：看着像公网，外面其实连不进来
		if b[0] == 100 && b[1] >= 64 && b[1] <= 127 {
			return false
		}
	}
	return true
}
