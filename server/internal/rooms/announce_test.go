package rooms_test

import (
	"encoding/json"
	"strings"
	"testing"
	"time"

	"knock/internal/rooms"
)

// 房主的 EndpointId 形状：32 字节 hex。
const testHostID = "abababababababababababababababababababababababababababababababab"

func newServiceWithToken(t *testing.T) (*rooms.Service, rooms.Room, string) {
	t.Helper()
	svc := rooms.NewService(rooms.NewMemoryStore(), 10*time.Minute)
	token, err := rooms.NewToken()
	if err != nil {
		t.Fatalf("生成令牌失败: %v", err)
	}
	room, err := svc.Create(25565, rooms.WithToken(token))
	if err != nil {
		t.Fatalf("开门失败: %v", err)
	}
	return svc, room, token
}

func TestAnnounceStoresHostAndCandidates(t *testing.T) {
	svc, room, token := newServiceWithToken(t)

	addrs := []string{"203.0.113.7:5000", "203.0.113.7:5001"}
	updated, err := svc.Announce(room.Code, token, testHostID, addrs)
	if err != nil {
		t.Fatalf("announce 失败: %v", err)
	}
	if updated.HostID != testHostID {
		t.Fatalf("host_id = %q", updated.HostID)
	}
	if len(updated.HostAddrs) != 2 {
		t.Fatalf("候选地址 = %v", updated.HostAddrs)
	}

	// 房客查码也要能拿到
	got, err := svc.Lookup(room.Code)
	if err != nil {
		t.Fatalf("查码失败: %v", err)
	}
	if got.HostID != testHostID || len(got.HostAddrs) != 2 {
		t.Fatalf("查码结果里没有房主信息: %+v", got)
	}
}

// 令牌是唯一的凭据：猜错的、没令牌的房间，一律不许写。
func TestAnnounceRequiresTheRightToken(t *testing.T) {
	svc, room, token := newServiceWithToken(t)
	addrs := []string{"203.0.113.7:5000"}

	if _, err := svc.Announce(room.Code, "deadbeef", testHostID, addrs); err != rooms.ErrBadToken {
		t.Fatalf("错令牌应当 ErrBadToken，实际 %v", err)
	}

	// 开门时没绑令牌的房间永远不能被 announce——否则任何人都能往里塞地址
	plain, err := svc.Create(25565)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := svc.Announce(plain.Code, "", testHostID, addrs); err != rooms.ErrBadToken {
		t.Fatalf("没有令牌的房间应当 ErrBadToken，实际 %v", err)
	}

	// 用对了才行
	if _, err := svc.Announce(room.Code, token, testHostID, addrs); err != nil {
		t.Fatalf("正确令牌应当成功: %v", err)
	}
}

func TestAnnounceOnUnknownRoomIsNotFound(t *testing.T) {
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute)
	if _, err := svc.Announce("AAAAAA", "whatever", testHostID, []string{"203.0.113.7:1"}); err != rooms.ErrNotFound {
		t.Fatalf("不存在的房间应当 ErrNotFound，实际 %v", err)
	}
}

func TestAnnounceValidatesInput(t *testing.T) {
	svc, room, token := newServiceWithToken(t)

	if _, err := svc.Announce(room.Code, token, "tooshort", []string{"203.0.113.7:5000"}); err != rooms.ErrBadHostID {
		t.Fatalf("短 host_id 应当 ErrBadHostID，实际 %v", err)
	}
	if _, err := svc.Announce(room.Code, token, testHostID, nil); err != rooms.ErrBadCandidates {
		t.Fatalf("没有候选地址应当 ErrBadCandidates，实际 %v", err)
	}
	if _, err := svc.Announce(room.Code, token, testHostID, []string{"203.0.113.7"}); err != rooms.ErrBadCandidates {
		t.Fatalf("缺端口的候选地址应当 ErrBadCandidates，实际 %v", err)
	}

	tooMany := make([]string, rooms.MaxHostAddrs+1)
	for i := range tooMany {
		tooMany[i] = "203.0.113.7:5000"
	}
	// 全是重复的会被去重成 1 个，所以这里用不同端口
	for i := range tooMany {
		tooMany[i] = "203.0.113.7:" + itoa(5000+i)
	}
	if _, err := svc.Announce(room.Code, token, testHostID, tooMany); err != rooms.ErrBadCandidates {
		t.Fatalf("超过上限应当 ErrBadCandidates，实际 %v", err)
	}
}

func TestAnnounceDeduplicatesAndIsRateLimited(t *testing.T) {
	svc, room, token := newServiceWithToken(t)

	updated, err := svc.Announce(room.Code, token, testHostID, []string{"203.0.113.7:5000", "203.0.113.7:5000"})
	if err != nil {
		t.Fatalf("announce 失败: %v", err)
	}
	if len(updated.HostAddrs) != 1 {
		t.Fatalf("重复地址应当去重，实际 %v", updated.HostAddrs)
	}

	// 紧接着再发一次：太频繁，应当被挡住
	if _, err := svc.Announce(room.Code, token, testHostID, []string{"203.0.113.7:5000"}); err != rooms.ErrTooFrequent {
		t.Fatalf("立即重发应当 ErrTooFrequent，实际 %v", err)
	}
}

func TestAnnounceRefusedAfterExpiry(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute).Clock(c.now)

	token, _ := rooms.NewToken()
	room, err := svc.Create(25565, rooms.WithToken(token))
	if err != nil {
		t.Fatal(err)
	}

	c.advance(2 * time.Minute)
	if _, err := svc.Announce(room.Code, token, testHostID, []string{"203.0.113.7:5000"}); err != rooms.ErrNotFound {
		t.Fatalf("过期房间应当 ErrNotFound，实际 %v", err)
	}
}

// 令牌哈希绝不能出现在任何会被序列化的地方。
func TestTokenHashNeverSerializes(t *testing.T) {
	_, room, _ := newServiceWithToken(t)

	raw, err := json.Marshal(room)
	if err != nil {
		t.Fatalf("序列化失败: %v", err)
	}
	if strings.Contains(string(raw), "token") {
		t.Fatalf("房间 JSON 里出现了 token 相关字段: %s", raw)
	}
	if len(room.TokenHash) != 32 {
		t.Fatalf("哈希长度 = %d，想要 32", len(room.TokenHash))
	}
}

func TestTokensAreUnique(t *testing.T) {
	seen := make(map[string]struct{}, 100)
	for i := 0; i < 100; i++ {
		token, err := rooms.NewToken()
		if err != nil {
			t.Fatal(err)
		}
		if len(token) != rooms.TokenLen*2 {
			t.Fatalf("令牌长度 = %d，想要 %d", len(token), rooms.TokenLen*2)
		}
		if _, dup := seen[token]; dup {
			t.Fatal("令牌重复了")
		}
		seen[token] = struct{}{}
	}
}

func itoa(v int) string {
	if v == 0 {
		return "0"
	}
	var buf [8]byte
	i := len(buf)
	for v > 0 {
		i--
		buf[i] = byte('0' + v%10)
		v /= 10
	}
	return string(buf[i:])
}

// 发布兼作心跳：房主每 60 秒发一次，房间就一直活着，不会"10 分钟到了"。
func TestAnnounceKeepsTheRoomAlive(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), 10*time.Minute).Clock(c.now)

	svc.SetMinIntervalForTest(0)
	token, _ := rooms.NewToken()
	room, err := svc.Create(25565, rooms.WithToken(token))
	if err != nil {
		t.Fatal(err)
	}

	// 9 分钟时打一次心跳，寿命应当从这一刻重新算
	c.advance(9 * time.Minute)
	if _, err := svc.Announce(room.Code, token, testHostID, []string{"203.0.113.7:5000"}); err != nil {
		t.Fatalf("心跳失败: %v", err)
	}

	// 再走 9 分钟（离开门已 18 分钟）：没心跳的话早就过期了
	c.advance(9 * time.Minute)
	if _, err := svc.Lookup(room.Code); err != nil {
		t.Fatalf("打了心跳之后房间还该在: %v", err)
	}

	// 心跳停了，超过寿命就该关
	c.advance(11 * time.Minute)
	if _, err := svc.Lookup(room.Code); err != rooms.ErrNotFound {
		t.Fatalf("心跳停了应当过期，实际 %v", err)
	}
}
