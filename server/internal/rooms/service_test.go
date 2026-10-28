package rooms_test

import (
	"sync"
	"testing"
	"time"

	"knock/internal/rooms"
)

// clock 是可推进的假时钟，让"过期"这件事不用 sleep 就能测。
type clock struct {
	mu sync.Mutex
	t  time.Time
}

func newClock() *clock { return &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)} }

func (c *clock) now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.t
}

func (c *clock) advance(d time.Duration) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.t = c.t.Add(d)
}

func newService(t *testing.T) (*rooms.Service, *clock) {
	t.Helper()
	c := newClock()
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute).Clock(c.now)
	return svc, c
}

// 字母表在测试里**硬写一遍**，不去 import code32——
// 否则就是拿实现测实现，改了表也测不出来。
const alphabet = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"

func TestCreateReturnsSixCharCodeFromAlphabet(t *testing.T) {
	svc, _ := newService(t)

	room, err := svc.Create(25565)
	if err != nil {
		t.Fatalf("开房失败: %v", err)
	}
	if len(room.Code) != rooms.CodeLen {
		t.Fatalf("门牌号长度 = %d，想要 %d（%q）", len(room.Code), rooms.CodeLen, room.Code)
	}
	for i := 0; i < len(room.Code); i++ {
		if !containsByte(alphabet, room.Code[i]) {
			t.Fatalf("门牌号 %q 含字母表外的字符 %q", room.Code, room.Code[i])
		}
	}
	if room.Port != 25565 {
		t.Fatalf("端口 = %d，想要 25565", room.Port)
	}
}

func TestCreateRejectsInvalidPort(t *testing.T) {
	svc, _ := newService(t)

	for _, port := range []int{0, -1, 65536} {
		if _, err := svc.Create(port); err != rooms.ErrInvalidPort {
			t.Fatalf("端口 %d 应当被拒绝，实际 err=%v", port, err)
		}
	}
}

func TestCreateStampsExpiryFromTTL(t *testing.T) {
	svc, c := newService(t)

	room, err := svc.Create(25565)
	if err != nil {
		t.Fatalf("开房失败: %v", err)
	}
	if got, want := room.ExpiresAt, c.now().Add(time.Minute); !got.Equal(want) {
		t.Fatalf("过期时间 = %v，想要 %v", got, want)
	}
}

func TestLookupFindsRoom(t *testing.T) {
	svc, _ := newService(t)

	created, err := svc.Create(25565)
	if err != nil {
		t.Fatalf("开房失败: %v", err)
	}

	got, err := svc.Lookup(created.Code)
	if err != nil {
		t.Fatalf("查码失败: %v", err)
	}
	if got.Code != created.Code {
		t.Fatalf("查到的房间 = %q，想要 %q", got.Code, created.Code)
	}
}

func TestLookupUnknownIsNotFound(t *testing.T) {
	svc, _ := newService(t)

	if _, err := svc.Lookup("AAAAAA"); err != rooms.ErrNotFound {
		t.Fatalf("不存在的码应当返回 ErrNotFound，实际 err=%v", err)
	}
}

// 门牌号的归一化（去空白/转大写）是 HTTP 层的职责，Service 只认大写形状。
// 这个测试把边界钉住，免得以后有人把 ToUpper 挪进来。
func TestLookupMalformedIsNotFound(t *testing.T) {
	svc, _ := newService(t)

	for _, code := range []string{"", "ABC", "ABCDEFG", "abcdef", "ABCUEF"} {
		if _, err := svc.Lookup(code); err != rooms.ErrNotFound {
			t.Fatalf("%q 应当被视为找不到，实际 err=%v", code, err)
		}
	}
}

func TestExpiredRoomIsNotFound(t *testing.T) {
	svc, c := newService(t)

	room, err := svc.Create(25565)
	if err != nil {
		t.Fatalf("开房失败: %v", err)
	}

	c.advance(2 * time.Minute) // 寿命只有 1 分钟

	if _, err := svc.Lookup(room.Code); err != rooms.ErrNotFound {
		t.Fatalf("过期房间应当找不到，实际 err=%v", err)
	}
	// 顺手被删掉，不等后台清理
	if n := svc.Len(); n != 0 {
		t.Fatalf("过期房间应当被顺手删除，仍有 %d 个", n)
	}
}

func TestSweepRemovesOnlyExpired(t *testing.T) {
	c := newClock()
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute).Clock(c.now)

	old, err := svc.Create(25565)
	if err != nil {
		t.Fatalf("开房失败: %v", err)
	}
	c.advance(90 * time.Second)
	fresh, err := svc.Create(25565)
	if err != nil {
		t.Fatalf("开房失败: %v", err)
	}

	if n := svc.Sweep(); n != 1 {
		t.Fatalf("应当清掉 1 个，实际 %d", n)
	}
	if _, err := svc.Lookup(fresh.Code); err != nil {
		t.Fatalf("没过期的房间被误删: %v", err)
	}
	if _, err := svc.Lookup(old.Code); err != rooms.ErrNotFound {
		t.Fatalf("过期房间还在: %v", err)
	}
}

func TestCloseRemovesRoom(t *testing.T) {
	svc, _ := newService(t)

	room, err := svc.Create(25565)
	if err != nil {
		t.Fatalf("开房失败: %v", err)
	}
	if err := svc.Close(room.Code); err != nil {
		t.Fatalf("关门失败: %v", err)
	}
	if _, err := svc.Lookup(room.Code); err != rooms.ErrNotFound {
		t.Fatalf("关掉的房间还能查到")
	}
	if err := svc.Close(room.Code); err != rooms.ErrNotFound {
		t.Fatalf("重复关门应当返回 ErrNotFound，实际 err=%v", err)
	}
}

func TestCodesAreUnique(t *testing.T) {
	svc, _ := newService(t)

	seen := make(map[string]struct{}, 200)
	for i := 0; i < 200; i++ {
		room, err := svc.Create(25565)
		if err != nil {
			t.Fatalf("第 %d 次开房失败: %v", i, err)
		}
		if _, dup := seen[room.Code]; dup {
			t.Fatalf("门牌号撞了: %q", room.Code)
		}
		seen[room.Code] = struct{}{}
	}
}

func containsByte(s string, b byte) bool {
	for i := 0; i < len(s); i++ {
		if s[i] == b {
			return true
		}
	}
	return false
}

// 房间上限是节点主人的自保手段：一台公网节点被刷满就谁都开不了门。
func TestCreateRejectsWhenNodeIsFull(t *testing.T) {
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute, rooms.WithMaxRooms(2))

	for i := 0; i < 2; i++ {
		if _, err := svc.Create(25565); err != nil {
			t.Fatalf("第 %d 次开门不该失败: %v", i+1, err)
		}
	}

	if _, err := svc.Create(25565); err != rooms.ErrNodeFull {
		t.Fatalf("满了应当返回 ErrNodeFull，实际 %v", err)
	}
}

// 满了以后先清一次过期房间，不能因为"还没扫到"就把人挡在门外。
func TestFullNodeMakesRoomForExpiredRooms(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute, rooms.WithMaxRooms(1)).Clock(c.now)

	if _, err := svc.Create(25565); err != nil {
		t.Fatalf("第一次开门失败: %v", err)
	}
	if _, err := svc.Create(25565); err != rooms.ErrNodeFull {
		t.Fatalf("满员时应当拒绝，实际 %v", err)
	}

	// 等第一间过期，再开门应当能成功（Create 内部会顺手清理）
	c.advance(2 * time.Minute)
	if _, err := svc.Create(25565); err != nil {
		t.Fatalf("过期房间应当腾出位置，实际 %v", err)
	}
}

// 0 表示不限制（本机调试用），别把默认行为悄悄改掉。
func TestZeroMaxRoomsMeansUnlimited(t *testing.T) {
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute, rooms.WithMaxRooms(0))
	for i := 0; i < 50; i++ {
		if _, err := svc.Create(25565); err != nil {
			t.Fatalf("第 %d 次开门失败: %v", i+1, err)
		}
	}
}
