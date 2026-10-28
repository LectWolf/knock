// Package rooms 是节点的房间登记领域：门牌号、房间、存储与服务。
//
// 语义边界（见 docs/GLOSSARY.md）：
//   - Room 是一次「门开着」的登记，有生命周期；它不等于 MC 服务端，也不等于一次连接。
//   - Code 是房间在**这个节点内**的短地址，不是凭据。
package rooms

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"time"

	"knock/internal/code32"
)

// CodeLen 是门牌号长度。6 位 × 32 字符表 ≈ 10.7 亿种，配合 TTL 与查询限速足够。
const CodeLen = 6

// ErrNotFound 表示门牌号不存在**或**已过期。
// 两者合并是刻意的：对外是同一句话（code_not_found），调用方也没有不同的动作可做。
var ErrNotFound = errors.New("rooms: 门牌号不存在或已过期")

// ErrInvalidPort 表示端口不在 1..65535。
var ErrInvalidPort = errors.New("rooms: 端口不合法")

// ErrNodeFull 表示节点上的房间已经到上限。
// 与 ErrNotFound 分开是为了让上层能说不同的话：一个是"你敲错了"，
// 一个是"这个节点满了，换一个节点"。
var ErrNodeFull = errors.New("rooms: 节点房间已满")

// ErrBadToken 表示房间令牌不对，或者这个房间压根没有令牌。
//
// "没有令牌"也算错：房间一旦不能证明归属，就不能接受 announce——
// 否则任何人都能往别人的房间里塞地址。
var ErrBadToken = errors.New("rooms: 房间令牌不对")

// ErrTooFrequent 表示两次发布间隔太短。
//
// 发布兼作心跳：房主每 60 秒重发一次，房间就一直活着。
// 所以这里限的是**频率**，不是总次数——次数限制会把正常心跳打死。
var ErrTooFrequent = errors.New("rooms: 发布太频繁")

// ErrBadHostID 表示房主的 EndpointId 形状不对（必须是 32 字节的 hex，64 个字符）。
var ErrBadHostID = errors.New("rooms: 房主身份不合法")

// ErrBadCandidates 表示候选地址不合法（要求 ip:port，最多 8 个）。
var ErrBadCandidates = errors.New("rooms: 候选地址不合法")

const (
	// TokenLen 是房间令牌的字节数。32 字节随机，猜不出来。
	TokenLen = 32

	// MaxHostAddrs 是一次 announce 最多带几个候选地址。
	MaxHostAddrs = 8

	// MinAnnounceInterval 是两次发布之间的最小间隔。
	// 心跳是 60 秒一次，5 秒足够挡住连点，也不会误伤正常刷新。
	MinAnnounceInterval = 5 * time.Second
)

// NewToken 生成一把房间令牌。明文只返回这一次。
func NewToken() (string, error) {
	buf := make([]byte, TokenLen)
	if _, err := rand.Read(buf); err != nil {
		return "", err
	}
	return hex.EncodeToString(buf), nil
}

// hashToken 是节点唯一会留下的痕迹。
func hashToken(token string) []byte {
	sum := sha256.Sum256([]byte(token))
	return sum[:]
}

// Room 是一次「门开着」的登记。
type Room struct {
	Code      string    `json:"code"`
	Port      int       `json:"port"`
	CreatedAt time.Time `json:"created_at"`
	ExpiresAt time.Time `json:"expires_at"`

	// TokenHash 是「谁能往这个房间发布候选地址」的凭据哈希。
	//
	// 明文令牌只在开门那一次给房主，节点**只存哈希**：
	// 这样即使节点被读库，也伪造不了别人的 announce。
	// json:"-" 是硬要求——它绝不能出现在查码响应里。
	TokenHash []byte `json:"-"`

	// 房主发布的 iroh 身份与直连地址（announce）。房客查码拿到它才能去敲门。
	HostID      string    `json:"host_id,omitempty"`
	HostAddrs   []string  `json:"host_addrs,omitempty"`
	AnnouncedAt time.Time `json:"announced_at,omitempty"`

	// lastAnnounce 是最后一次发布（心跳）的时刻，用来限频。
	lastAnnounce time.Time
}

// Expired 报告在 now 时刻房间是否已经过期。
func (r Room) Expired(now time.Time) bool { return !now.Before(r.ExpiresAt) }

// NewCode 生成一个门牌号。碰撞由调用方重试处理。
//
// 字母表来自 code32（与节点码同一张表）：只允许有一套字符表，
// 否则"门牌号里能不能出现 0"这种事会变成两个包各说各话。
func NewCode() (string, error) {
	buf := make([]byte, CodeLen)
	if _, err := rand.Read(buf); err != nil {
		return "", err
	}
	out := make([]byte, CodeLen)
	for i, b := range buf {
		// 32 整除 256，取模没有偏置
		out[i] = code32.Alphabet[int(b)%len(code32.Alphabet)]
	}
	return string(out), nil
}

// NormalizeCode 把用户输入归一化：去分隔符、转大写、O/I/L 走 Crockford 别名。
// 归一是 HTTP 层的职责，Service 只认规范形式。
func NormalizeCode(raw string) string { return code32.Normalize(raw) }

// ValidCode 只做形状校验：长度与字母表。入参必须是已归一化的规范形式。
func ValidCode(code string) bool {
	return len(code) == CodeLen && code32.Valid(code)
}
