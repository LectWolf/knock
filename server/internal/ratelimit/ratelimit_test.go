package ratelimit_test

import (
	"testing"
	"time"

	"knock/internal/ratelimit"
)

type fakeClock struct{ t time.Time }

func (c *fakeClock) now() time.Time          { return c.t }
func (c *fakeClock) advance(d time.Duration) { c.t = c.t.Add(d) }

func newLimiter(limit int, window time.Duration) (*ratelimit.Limiter, *fakeClock) {
	clock := &fakeClock{t: time.Unix(1_700_000_000, 0)}
	return ratelimit.NewWithClock(limit, window, clock.now), clock
}

func TestAllowsUpToLimitThenDenies(t *testing.T) {
	l, _ := newLimiter(3, time.Minute)

	for i := 0; i < 3; i++ {
		if ok, _ := l.Allow("1.2.3.4"); !ok {
			t.Fatalf("第 %d 次应当放行", i+1)
		}
	}
	ok, retry := l.Allow("1.2.3.4")
	if ok {
		t.Fatal("第 4 次应当被拒")
	}
	if retry <= 0 || retry > time.Minute {
		t.Fatalf("重试等待时间不合理：%s", retry)
	}
}

func TestWindowRollsOver(t *testing.T) {
	l, clock := newLimiter(2, time.Minute)

	l.Allow("a")
	l.Allow("a")
	if ok, _ := l.Allow("a"); ok {
		t.Fatal("窗口内第三次应当被拒")
	}

	clock.advance(time.Minute)
	if ok, _ := l.Allow("a"); !ok {
		t.Fatal("窗口过去后应当重新放行")
	}
}

func TestKeysAreIndependent(t *testing.T) {
	l, _ := newLimiter(1, time.Minute)

	if ok, _ := l.Allow("a"); !ok {
		t.Fatal("a 第一次应当放行")
	}
	if ok, _ := l.Allow("b"); !ok {
		t.Fatal("b 不应受 a 影响")
	}
	if ok, _ := l.Allow("a"); ok {
		t.Fatal("a 第二次应当被拒")
	}
}

func TestDisabledLimiterAllowsEverything(t *testing.T) {
	l := ratelimit.New(0, time.Minute)
	for i := 0; i < 100; i++ {
		if ok, _ := l.Allow("x"); !ok {
			t.Fatal("limit=0 表示不限制")
		}
	}
	if l.Len() != 0 {
		t.Fatalf("不限制时不该记录 key，实际 %d 个", l.Len())
	}
}

func TestSweepDropsStaleBuckets(t *testing.T) {
	l, clock := newLimiter(5, time.Minute)

	l.Allow("a")
	l.Allow("b")
	if l.Len() != 2 {
		t.Fatalf("Len = %d，想要 2", l.Len())
	}

	clock.advance(2 * time.Minute)
	if removed := l.Sweep(); removed != 2 {
		t.Fatalf("Sweep 清掉 %d 个，想要 2", removed)
	}
	if l.Len() != 0 {
		t.Fatalf("清完还剩 %d 个", l.Len())
	}
}

func TestNilLimiterIsSafe(t *testing.T) {
	var l *ratelimit.Limiter
	if ok, _ := l.Allow("a"); !ok {
		t.Fatal("nil 限速器应当放行（等价于不限制）")
	}
	if l.Len() != 0 {
		t.Fatal("nil 的 Len 应当是 0")
	}
	if l.Sweep() != 0 {
		t.Fatal("nil 的 Sweep 应当是 0")
	}
}
