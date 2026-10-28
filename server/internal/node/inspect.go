package node

import (
	"fmt"
	"strings"

	"knock/internal/invite"
)

// Inspect 把用户粘进来的任意一串东西翻译成人话。
//
// 存在的理由：节点主人收到的十有八九是"这串东西到底对不对"。
// 与其让人猜，不如让命令行直接说是哪种形态、缺什么、指纹是多少。
// 这个函数也是解码路径的端到端验证入口（-inspect 走的就是它）。
func Inspect(input string) (string, error) {
	parsed, err := invite.Parse(input)
	if err != nil {
		return "", err
	}

	var b strings.Builder
	switch parsed.Kind {
	case invite.RoomOnly:
		b.WriteString("这是一串门牌号：一个房间的短址，不是节点。\n\n")
		fmt.Fprintf(&b, "门牌号：%s\n\n", parsed.Room)
		b.WriteString("光有它还敲不成门——门牌号只在某一个节点上才有意义。\n")
		b.WriteString("完整的邀请形如「节点码.门牌号」。\n")

	case invite.Short:
		b.WriteString("这是一条短邀请：用 4 个字符的标签指向一个节点。\n\n")
		fmt.Fprintf(&b, "标签：%s\n", parsed.Label)
		fmt.Fprintf(&b, "门牌号：%s\n\n", parsed.Room)
		b.WriteString("标签只有 16 bit，脱离「你已经添加过哪些节点」就定位不到具体机器。\n")
		b.WriteString("所以它只适合对方此前已经加过这个节点的场合。\n")

	case invite.Full:
		b.WriteString("这是一条完整邀请：节点 + 门牌号，拿到就能敲门。\n\n")
		b.WriteString(nodeSummary(parsed.Node))
		fmt.Fprintf(&b, "\n门牌号：%s\n", parsed.Room)
		b.WriteString("\n如果敲不开，先看门牌号是不是已经过期（默认 10 分钟）。\n")

	case invite.NodeOnly:
		b.WriteString("这是一个节点码：只说明「有这么一台节点」，不含房间。\n")
		b.WriteString("拿到它等于添加了这个节点，但还没有可敲的门。\n\n")
		b.WriteString(nodeSummary(parsed.Node))
	}

	return b.String(), nil
}

func nodeSummary(node *invite.NodeCode) string {
	if node == nil {
		return ""
	}

	var b strings.Builder
	fmt.Fprintf(&b, "标签：%s\n", node.LabelChars())
	fmt.Fprintf(&b, "端口：%d\n", node.Port)
	fmt.Fprintf(&b, "指纹：%x\n", node.Pin)
	if node.HasIPv4 {
		fmt.Fprintf(&b, "IPv4：%d.%d.%d.%d\n", node.IPv4[0], node.IPv4[1], node.IPv4[2], node.IPv4[3])
	}
	if node.HasIPv6 {
		fmt.Fprintf(&b, "IPv6：%x\n", node.IPv6[:])
	}
	if node.Domain != "" {
		fmt.Fprintf(&b, "域名：%s\n", node.Domain)
	}
	fmt.Fprintf(&b, "允许中转：%s\n", onOff(node.RelayAllowed))
	return b.String()
}
