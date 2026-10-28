package rooms

import (
	"crypto/subtle"
	"encoding/hex"
	"net/netip"
	"strings"
	"time"
)

// DefaultTTL 是房间寿命。开门后 10 分钟内没人敲，门自动关上。
const DefaultTTL = 10 * time.Minute

// maxCodeAttempts 是撞码重试次数。6 位码在正常房间量级下几乎不会撞。
const maxCodeAttempts = 8

// Service 是房间登记的所有领域规则：发码、查码、关门、清理过期。
type Service struct {
	store    Store
	ttl      time.Duration
	maxRooms int
	now      func() time.Time
	// minAnnounce 是发布限频；0 表示不限（只在测试里这么用）
	minAnnounce time.Duration
	newCode     func() (string, error)
}

// Option 是给 Service 的可选配置。用变参而不改构造签名，
// 是为了让"只是想要默认行为"的调用方继续写 NewService(store, ttl)。
type Option func(*Service)

// WithMaxRooms 限制同时开着的房间数；n <= 0 表示不限制。
//
// 这是节点主人的自保手段：一台公网节点被刷满房间就谁都开不了门了。
func WithMaxRooms(n int) Option {
	return func(s *Service) { s.maxRooms = n }
}

func NewService(store Store, ttl time.Duration, opts ...Option) *Service {
	if ttl <= 0 {
		ttl = DefaultTTL
	}
	s := &Service{
		store:       store,
		ttl:         ttl,
		now:         time.Now,
		newCode:     NewCode,
		minAnnounce: MinAnnounceInterval,
	}
	for _, opt := range opts {
		if opt != nil {
			opt(s)
		}
	}
	return s
}

// SetMinIntervalForTest 关掉发布限频，只给测试用。
func (s *Service) SetMinIntervalForTest(d time.Duration) { s.minAnnounce = d }

// Clock 注入时间源，只给测试用。返回自身便于链式调用。
func (s *Service) Clock(now func() time.Time) *Service {
	if now != nil {
		s.now = now
	}
	return s
}

// Create 登记一个新房间并返回它。房主「开门」就是调这个。
//
// 用变参而不是多返回值带令牌：房间和令牌是**两件东西**——
// 房间会出现在查码响应里，令牌只该出现在开门那一次。
// 不传 WithToken 也能开房，但那样的房间永远无法 announce（见 Service.Announce）。
func (s *Service) Create(port int, opts ...CreateOption) (Room, error) {
	if port < 1 || port > 65535 {
		return Room{}, ErrInvalidPort
	}

	if s.maxRooms > 0 && s.store.Len() >= s.maxRooms {
		// 已经满的时候先清一次过期房间：不能因为"还没来得及扫"就把人挡在门外
		s.Sweep()
		if s.store.Len() >= s.maxRooms {
			return Room{}, ErrNodeFull
		}
	}

	var cfg createOptions
	for _, opt := range opts {
		if opt != nil {
			opt(&cfg)
		}
	}

	now := s.now()
	for attempt := 0; attempt < maxCodeAttempts; attempt++ {
		code, err := s.newCode()
		if err != nil {
			return Room{}, err
		}
		if _, exists := s.store.Get(code); exists {
			continue
		}
		room := Room{
			Code:      code,
			Port:      port,
			CreatedAt: now,
			ExpiresAt: now.Add(s.ttl),
		}
		if cfg.token != "" {
			room.TokenHash = hashToken(cfg.token)
		}
		if err := s.store.Put(room); err != nil {
			return Room{}, err
		}
		return room, nil
	}
	return Room{}, ErrNotFound // 撞码 8 次：码空间或随机源有问题
}

// CreateOption 是开房时的可选参数。
type CreateOption func(*createOptions)

type createOptions struct{ token string }

// WithToken 给这个房间绑一把令牌。持有令牌的人才能 announce 候选地址。
func WithToken(token string) CreateOption {
	return func(o *createOptions) { o.token = token }
}

// Announce 让房主把自己的 iroh 身份与直连地址放到房间上。
//
// 这是"敲门"能成立的关键一步：房客查码时拿到的不是 IP，而是房主的 EndpointId
// 加上一组候选地址，然后逐个去试。
//
// 只认开门时给的那把令牌——否则任何人都能往别人的房间里塞地址，
// 把人骗去连自己。令牌用常量时间比较，不给自己留时序侧信道。
func (s *Service) Announce(code, token, hostID string, addrs []string) (Room, error) {
	if !ValidCode(code) {
		return Room{}, ErrNotFound
	}

	room, ok := s.store.Get(code)
	if !ok {
		return Room{}, ErrNotFound
	}
	if room.Expired(s.now()) {
		s.store.Delete(code)
		return Room{}, ErrNotFound
	}

	if len(room.TokenHash) == 0 || subtle.ConstantTimeCompare(room.TokenHash, hashToken(token)) != 1 {
		return Room{}, ErrBadToken
	}
	if min := s.minAnnounce; !room.lastAnnounce.IsZero() && min > 0 && s.now().Sub(room.lastAnnounce) < min {
		return Room{}, ErrTooFrequent
	}

	cleaned := make([]string, 0, len(addrs))
	seen := make(map[string]struct{}, len(addrs))
	for _, raw := range addrs {
		addr, err := netip.ParseAddrPort(raw)
		if err != nil {
			return Room{}, ErrBadCandidates
		}
		canonical := addr.String()
		if _, dup := seen[canonical]; dup {
			continue
		}
		seen[canonical] = struct{}{}
		cleaned = append(cleaned, canonical)
	}
	if len(cleaned) == 0 || len(cleaned) > MaxHostAddrs {
		return Room{}, ErrBadCandidates
	}

	normalizedID, err := normalizeHostID(hostID)
	if err != nil {
		return Room{}, err
	}

	now := s.now()
	room.HostID = normalizedID
	room.HostAddrs = cleaned
	room.AnnouncedAt = now
	room.lastAnnounce = now
	// 发布兼作心跳：房间寿命从**最后一次**心跳算，房主还在门就一直开着
	room.ExpiresAt = now.Add(s.ttl)
	if err := s.store.Put(room); err != nil {
		return Room{}, err
	}
	return room, nil
}

// normalizeHostID 校验并规范化房主的 EndpointId：32 字节、hex、小写。
func normalizeHostID(hostID string) (string, error) {
	raw, err := hex.DecodeString(strings.TrimSpace(hostID))
	if err != nil || len(raw) != 32 {
		return "", ErrBadHostID
	}
	return strings.ToLower(strings.TrimSpace(hostID)), nil
}

// Lookup 按门牌号查房间。过期的房间顺手删掉再报找不到。
func (s *Service) Lookup(code string) (Room, error) {
	if !ValidCode(code) {
		return Room{}, ErrNotFound
	}
	room, ok := s.store.Get(code)
	if !ok {
		return Room{}, ErrNotFound
	}
	if room.Expired(s.now()) {
		s.store.Delete(code)
		return Room{}, ErrNotFound
	}
	return room, nil
}

// Close 主动关门（房主点「关门」，或对端已结束后的收尾）。
func (s *Service) Close(code string) error {
	if s.store.Delete(code) {
		return nil
	}
	return ErrNotFound
}

// Sweep 清掉所有过期房间，返回清掉的个数。由后台 ticker 调用。
func (s *Service) Sweep() int {
	now := s.now()
	n := 0
	for _, room := range s.store.List() {
		if room.Expired(now) {
			if s.store.Delete(room.Code) {
				n++
			}
		}
	}
	return n
}

// Len 是当前活着的房间数（给健康检查用）。
func (s *Service) Len() int { return s.store.Len() }
