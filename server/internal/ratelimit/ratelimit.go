// Package ratelimit 是节点用的固定窗口限速。
//
// 为什么节点必须有它：门牌号只有 6 位（约 10.7 亿种），不设限的话一个人就能
// 把整个空间扫一遍，把别人的门牌号捞出来。限速是**这个产品的安全前提**，
// 不是"顺手加的性能优化"（见 docs/proposals/self-hosted-relay.md §13）。
//
// 选固定窗口而不是令牌桶：节点是别人自托管的，行为要能被一眼看懂——
// "每分钟最多 30 次，超了就等 60 秒"比"令牌 0.5/s、桶容量 30"更好解释。
package ratelimit

import (
	"sync"
	"time"
)

// Limiter 按 key（通常是客户端 IP）做固定窗口限速。
//
// 零值不可用；用 New 构造。limit <= 0 表示不限制。
type Limiter struct {
	mu      sync.Mutex
	limit   int
	window  time.Duration
	now     func() time.Time
	buckets map[string]*bucket
}

type bucket struct {
	start time.Time
	count int
}

// New 造一个限速器：每个 key 在 window 内最多 limit 次。
func New(limit int, window time.Duration) *Limiter {
	return NewWithClock(limit, window, time.Now)
}

// NewWithClock 注入时间源，只给测试用。
func NewWithClock(limit int, window time.Duration, now func() time.Time) *Limiter {
	if now == nil {
		now = time.Now
	}
	return &Limiter{
		limit:   limit,
		window:  window,
		now:     now,
		buckets: make(map[string]*bucket),
	}
}

// Limit 是配置的上限，0 表示不限。
func (l *Limiter) Limit() int {
	if l == nil {
		return 0
	}
	return l.limit
}

// Allow 报告这次请求是否放行；不放行时给出还要等多久。
func (l *Limiter) Allow(key string) (bool, time.Duration) {
	if l == nil || l.limit <= 0 || l.window <= 0 {
		return true, 0
	}

	l.mu.Lock()
	defer l.mu.Unlock()

	now := l.now()
	b, ok := l.buckets[key]
	if !ok || now.Sub(b.start) >= l.window {
		l.buckets[key] = &bucket{start: now, count: 1}
		return true, 0
	}
	if b.count < l.limit {
		b.count++
		return true, 0
	}
	return false, l.window - now.Sub(b.start)
}

// Sweep 清掉已经过期的桶。没人调也不会错，只是 map 会慢慢变长——
// 公网上被打的时候 key 是无限的，所以后台必须定期调。
func (l *Limiter) Sweep() int {
	if l == nil || l.window <= 0 {
		return 0
	}
	l.mu.Lock()
	defer l.mu.Unlock()

	now := l.now()
	removed := 0
	for key, b := range l.buckets {
		if now.Sub(b.start) >= l.window {
			delete(l.buckets, key)
			removed++
		}
	}
	return removed
}

// Len 是当前记着的 key 个数（给健康检查和测试用）。
func (l *Limiter) Len() int {
	if l == nil {
		return 0
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	return len(l.buckets)
}
