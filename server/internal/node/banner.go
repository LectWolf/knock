package node

import (
	"fmt"
	"net/netip"
	"strings"

	"knock/internal/invite"
)

// Banner 是节点启动时给人看的那一屏。
//
// 单独抽出来是为了可测：这一屏是主人唯一会照抄的东西，
// 里面必须出现完整的节点码、短邀请的标签、以及可达性警告。
func Banner(cfg Config, code invite.NodeCode, codeText string) string {
	var b strings.Builder

	b.WriteString("\n—— 这个节点的编码 ——\n\n")
	b.WriteString("节点码（复制这一整串发给别人）：\n  ")
	b.WriteString(codeText)
	b.WriteString("\n\n")
	b.WriteString("短邀请（对方已添加过这个节点时，只需发）：\n  ")
	b.WriteString(code.LabelChars())
	b.WriteString(".<6 位门牌号>\n\n")

	fmt.Fprintf(&b, "标签：%s\n", code.LabelChars())
	fmt.Fprintf(&b, "端口：%d\n", code.Port)
	fmt.Fprintf(&b, "数据中转：%s\n", onOff(code.RelayAllowed))

	if code.HasIPv4 {
		fmt.Fprintf(&b, "IPv4：%s\n", netip.AddrFrom4(code.IPv4))
	}
	if code.HasIPv6 {
		fmt.Fprintf(&b, "IPv6：%s\n", netip.AddrFrom16(code.IPv6))
	}
	if code.Domain != "" {
		fmt.Fprintf(&b, "域名：%s\n", code.Domain)
	}

	if warning := cfg.ReachabilityWarning(); warning != "" {
		b.WriteString("\n⚠ ")
		b.WriteString(warning)
		b.WriteString("\n")
	}

	return b.String()
}

func onOff(v bool) string {
	if v {
		return "允许"
	}
	return "不允许（只做信令）"
}
