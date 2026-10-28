package httpapi_test

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"knock/internal/httpapi"
	"knock/internal/ratelimit"
	"knock/internal/rooms"
)

type clock struct {
	mu sync.Mutex
	t  time.Time
}

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

// newServer 起一个真实 HTTP 服务（httptest），测的是外部可见的行为。
func newServer(t *testing.T) (*httptest.Server, *clock) {
	t.Helper()
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute).Clock(c.now)
	srv := httptest.NewServer(httpapi.New(svc).Handler())
	t.Cleanup(srv.Close)
	return srv, c
}

type roomBody struct {
	Code       string    `json:"code"`
	Port       int       `json:"port"`
	CreatedAt  time.Time `json:"created_at"`
	ExpiresAt  time.Time `json:"expires_at"`
	TTLSeconds int       `json:"ttl_seconds"`

	Token     string   `json:"token"`
	NodeID    string   `json:"node_id"`
	HostID    string   `json:"host_id"`
	HostAddrs []string `json:"host_addrs"`
}

func postRoom(t *testing.T, base, body string) (*http.Response, roomBody) {
	t.Helper()
	resp, err := http.Post(base+"/rooms", "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST /rooms 失败: %v", err)
	}
	t.Cleanup(func() { resp.Body.Close() })

	var out roomBody
	if resp.StatusCode == http.StatusCreated {
		if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
			t.Fatalf("解析响应失败: %v", err)
		}
	}
	return resp, out
}

func TestCreateRoomReturnsSixDigitCode(t *testing.T) {
	srv, _ := newServer(t)

	resp, room := postRoom(t, srv.URL, `{"port":25565}`)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("状态码 = %d，想要 201", resp.StatusCode)
	}
	if len(room.Code) != rooms.CodeLen {
		t.Fatalf("门牌号 = %q，长度应当为 %d", room.Code, rooms.CodeLen)
	}
	if room.Port != 25565 {
		t.Fatalf("端口 = %d，想要 25565", room.Port)
	}
	if room.TTLSeconds != 60 {
		t.Fatalf("ttl_seconds = %d，想要 60", room.TTLSeconds)
	}
}

func TestCreateRoomDefaultsPort(t *testing.T) {
	srv, _ := newServer(t)

	resp, room := postRoom(t, srv.URL, `{}`)
	if resp.StatusCode != http.StatusCreated {
		t.Fatalf("状态码 = %d，想要 201", resp.StatusCode)
	}
	if room.Port != httpapi.DefaultPort {
		t.Fatalf("端口 = %d，想要默认 %d", room.Port, httpapi.DefaultPort)
	}
}

func TestCreateRoomRejectsBadJSON(t *testing.T) {
	srv, _ := newServer(t)

	resp, _ := postRoom(t, srv.URL, `not json`)
	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("状态码 = %d，想要 400", resp.StatusCode)
	}
}

func TestCreateRoomRejectsInvalidPort(t *testing.T) {
	srv, _ := newServer(t)

	for _, body := range []string{`{"port":-1}`, `{"port":70000}`} {
		resp, _ := postRoom(t, srv.URL, body)
		if resp.StatusCode != http.StatusBadRequest {
			t.Fatalf("%s 状态码 = %d，想要 400", body, resp.StatusCode)
		}
	}
}

func TestLookupRoom(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{"port":25566}`)

	resp, err := http.Get(srv.URL + "/rooms/" + created.Code)
	if err != nil {
		t.Fatalf("GET 失败: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("状态码 = %d，想要 200", resp.StatusCode)
	}
	var got roomBody
	if err := json.NewDecoder(resp.Body).Decode(&got); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	if got.Code != created.Code || got.Port != 25566 {
		t.Fatalf("查到 %+v，想要 code=%s port=25566", got, created.Code)
	}
}

func TestLookupIsCaseInsensitive(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	resp, err := http.Get(srv.URL + "/rooms/" + strings.ToLower(created.Code))
	if err != nil {
		t.Fatalf("GET 失败: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("小写门牌号也应当查到，状态码 = %d", resp.StatusCode)
	}
}

func TestLookupUnknownReturnsCodeNotFound(t *testing.T) {
	srv, _ := newServer(t)

	resp, err := http.Get(srv.URL + "/rooms/AAAAAA")
	if err != nil {
		t.Fatalf("GET 失败: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("状态码 = %d，想要 404", resp.StatusCode)
	}
	var body struct {
		Error string `json:"error"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	// 客户端靠这个字段映射到 FailReason::CodeNotFound，不能改
	if body.Error != "code_not_found" {
		t.Fatalf("error = %q，想要 code_not_found", body.Error)
	}
}

func TestLookupMalformedReturnsCodeNotFound(t *testing.T) {
	srv, _ := newServer(t)

	// 形状不对和不存在对用户是同一句话，所以刻意都返回 code_not_found
	for _, code := range []string{"ABC", "ABCDEFG", "ZZZZZ1"} {
		resp, err := http.Get(srv.URL + "/rooms/" + code)
		if err != nil {
			t.Fatalf("GET 失败: %v", err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusNotFound {
			t.Fatalf("%q 状态码 = %d，想要 404", code, resp.StatusCode)
		}
	}
}

func TestDeleteRoom(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	req, err := http.NewRequest(http.MethodDelete, srv.URL+"/rooms/"+created.Code, nil)
	if err != nil {
		t.Fatalf("构造请求失败: %v", err)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("DELETE 失败: %v", err)
	}
	resp.Body.Close()
	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("状态码 = %d，想要 204", resp.StatusCode)
	}

	again, err := http.Get(srv.URL + "/rooms/" + created.Code)
	if err != nil {
		t.Fatalf("GET 失败: %v", err)
	}
	again.Body.Close()
	if again.StatusCode != http.StatusNotFound {
		t.Fatalf("关门后还能查到，状态码 = %d", again.StatusCode)
	}
}

func TestExpiredRoomIsGone(t *testing.T) {
	srv, c := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	c.advance(2 * time.Minute)

	resp, err := http.Get(srv.URL + "/rooms/" + created.Code)
	if err != nil {
		t.Fatalf("GET 失败: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("过期房间状态码 = %d，想要 404", resp.StatusCode)
	}
}

func TestHealthz(t *testing.T) {
	srv, _ := newServer(t)

	// 顺便验一下计数：开一间房之后 healthz 应当看到 1 间
	postRoom(t, srv.URL, `{}`)

	resp, err := http.Get(srv.URL + "/healthz")
	if err != nil {
		t.Fatalf("GET 失败: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("状态码 = %d，想要 200", resp.StatusCode)
	}
	var body struct {
		Status string `json:"status"`
		Rooms  int    `json:"rooms"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
		t.Fatalf("解析失败: %v", err)
	}
	if body.Status != "ok" || body.Rooms != 1 {
		t.Fatalf("healthz = %+v，想要 status=ok rooms=1", body)
	}
}

func TestUnknownRouteIs404(t *testing.T) {
	srv, _ := newServer(t)

	resp, err := http.Get(srv.URL + "/nope")
	if err != nil {
		t.Fatalf("GET 失败: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("状态码 = %d，想要 404", resp.StatusCode)
	}
}

// ————————————————————————————————————————————————
// 滥用防护：限速与房间上限
// ————————————————————————————————————————————————

// newServerWith 允许测试自己决定服务与防护配置。
func newServerWith(t *testing.T, svc *rooms.Service, opts ...httpapi.Option) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(httpapi.New(svc, opts...).Handler())
	t.Cleanup(srv.Close)
	return srv
}

func get(t *testing.T, url string) *http.Response {
	t.Helper()
	resp, err := http.Get(url)
	if err != nil {
		t.Fatalf("GET %s 失败: %v", url, err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

func deleteReq(t *testing.T, url string) *http.Response {
	t.Helper()
	req, err := http.NewRequest(http.MethodDelete, url, nil)
	if err != nil {
		t.Fatalf("构造 DELETE 失败: %v", err)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("DELETE 失败: %v", err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

func errorCode(t *testing.T, resp *http.Response) string {
	t.Helper()
	var body struct {
		Error string `json:"error"`
	}
	if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
		t.Fatalf("解析错误响应失败: %v", err)
	}
	return body.Error
}

// 门牌号只有 6 位：不限速就等于允许别人把整个空间扫一遍。
func TestLookupIsRateLimited(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	// 房间寿命要比限速窗口长：否则"窗口过去后恢复"会被"房间过期"盖住，
	// 测出来的就是另一回事了
	svc := rooms.NewService(rooms.NewMemoryStore(), 10*time.Minute).Clock(c.now)
	lookup := ratelimit.NewWithClock(2, time.Minute, c.now)
	srv := newServerWith(t, svc, httpapi.WithLookupLimiter(lookup))

	_, created := postRoom(t, srv.URL, `{}`)

	for i := 0; i < 2; i++ {
		resp := get(t, srv.URL+"/rooms/"+created.Code)
		if resp.StatusCode != http.StatusOK {
			t.Fatalf("第 %d 次查码应当放行，状态码 = %d", i+1, resp.StatusCode)
		}
	}

	resp := get(t, srv.URL+"/rooms/"+created.Code)
	if resp.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("第 3 次查码应当 429，实际 %d", resp.StatusCode)
	}
	if got := resp.Header.Get("Retry-After"); got != "60" {
		t.Fatalf("Retry-After = %q，想要 60", got)
	}
	if code := errorCode(t, resp); code != "too_many_requests" {
		t.Fatalf("error = %q，想要 too_many_requests", code)
	}

	// 窗口过去后必须恢复，否则限速就变成"永久封禁"了
	c.advance(time.Minute)
	after := get(t, srv.URL+"/rooms/"+created.Code)
	if after.StatusCode != http.StatusOK {
		t.Fatalf("窗口过去后应当恢复，状态码 = %d", after.StatusCode)
	}
}

// 只限 GET 会被绕过：DELETE 同样能探测门牌号存不存在。
func TestDeleteSharesTheLookupLimit(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute).Clock(c.now)
	lookup := ratelimit.NewWithClock(1, time.Minute, c.now)
	srv := newServerWith(t, svc, httpapi.WithLookupLimiter(lookup))

	_, created := postRoom(t, srv.URL, `{}`)

	if resp := get(t, srv.URL+"/rooms/"+created.Code); resp.StatusCode != http.StatusOK {
		t.Fatalf("第一次查码应当放行，状态码 = %d", resp.StatusCode)
	}
	if resp := deleteReq(t, srv.URL+"/rooms/"+created.Code); resp.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("DELETE 应当共用查码配额，实际状态码 = %d", resp.StatusCode)
	}
}

func TestCreateIsRateLimited(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute).Clock(c.now)
	create := ratelimit.NewWithClock(1, time.Minute, c.now)
	srv := newServerWith(t, svc, httpapi.WithCreateLimiter(create))

	if resp, _ := postRoom(t, srv.URL, `{}`); resp.StatusCode != http.StatusCreated {
		t.Fatalf("第一次开门应当成功，状态码 = %d", resp.StatusCode)
	}

	resp, _ := postRoom(t, srv.URL, `{}`)
	if resp.StatusCode != http.StatusTooManyRequests {
		t.Fatalf("第二次开门应当 429，实际 %d", resp.StatusCode)
	}
}

// 节点满了要给一句能听懂的话，而不是 500。
func TestNodeFullReturns503(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute, rooms.WithMaxRooms(1)).Clock(c.now)
	srv := newServerWith(t, svc)

	if resp, _ := postRoom(t, srv.URL, `{}`); resp.StatusCode != http.StatusCreated {
		t.Fatalf("第一次开门应当成功，状态码 = %d", resp.StatusCode)
	}

	resp, _ := postRoom(t, srv.URL, `{}`)
	if resp.StatusCode != http.StatusServiceUnavailable {
		t.Fatalf("节点满了应当 503，实际 %d", resp.StatusCode)
	}
	if code := errorCode(t, resp); code != "node_full" {
		t.Fatalf("error = %q，想要 node_full", code)
	}
}

// 不配限速器时行为要和以前完全一致（本机调试、测试都靠这个）。
func TestNoLimiterMeansNoLimit(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	for i := 0; i < 50; i++ {
		if resp := get(t, srv.URL+"/rooms/"+created.Code); resp.StatusCode != http.StatusOK {
			t.Fatalf("第 %d 次查码应当放行，状态码 = %d", i+1, resp.StatusCode)
		}
	}
}

// ————————————————————————————————————————————————
// announce：房主把 iroh 身份与直连地址放到房间上
// ————————————————————————————————————————————————

const testHostID = "abababababababababababababababababababababababababababababababab"

func postJSON(t *testing.T, url, body string) *http.Response {
	t.Helper()
	resp, err := http.Post(url, "application/json", strings.NewReader(body))
	if err != nil {
		t.Fatalf("POST %s 失败: %v", url, err)
	}
	t.Cleanup(func() { resp.Body.Close() })
	return resp
}

func decodeRoom(t *testing.T, resp *http.Response) roomBody {
	t.Helper()
	var body roomBody
	if err := json.NewDecoder(resp.Body).Decode(&body); err != nil {
		t.Fatalf("解析房间失败: %v", err)
	}
	return body
}

// 令牌只在开门那一次给；查码绝不能带出来，否则拿到门牌号的人就能冒充房主。
func TestTokenOnlyAppearsWhenOpening(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	if created.Token == "" {
		t.Fatal("开门响应里应当有令牌")
	}

	looked := decodeRoom(t, get(t, srv.URL+"/rooms/"+created.Code))
	if looked.Token != "" {
		t.Fatalf("查码响应不该带令牌，实际 %q", looked.Token)
	}
}

func TestAnnounceThenLookupCarriesCandidates(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	body := `{"token":"` + created.Token + `","host_id":"` + testHostID + `","addrs":["203.0.113.7:5000","203.0.113.7:5001"]}`
	resp := postJSON(t, srv.URL+"/rooms/"+created.Code+"/announce", body)
	if resp.StatusCode != http.StatusOK {
		t.Fatalf("announce 状态码 = %d，想要 200", resp.StatusCode)
	}

	got := decodeRoom(t, get(t, srv.URL+"/rooms/"+created.Code))
	if got.HostID != testHostID {
		t.Fatalf("host_id = %q", got.HostID)
	}
	if len(got.HostAddrs) != 2 || got.HostAddrs[0] != "203.0.113.7:5000" {
		t.Fatalf("候选地址 = %v", got.HostAddrs)
	}
}

func TestAnnounceWithWrongTokenIsForbidden(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	body := `{"token":"deadbeef","host_id":"` + testHostID + `","addrs":["203.0.113.7:5000"]}`
	resp := postJSON(t, srv.URL+"/rooms/"+created.Code+"/announce", body)

	if resp.StatusCode != http.StatusForbidden {
		t.Fatalf("错令牌应当 403，实际 %d", resp.StatusCode)
	}
	if code := errorCode(t, resp); code != "bad_token" {
		t.Fatalf("error = %q，想要 bad_token", code)
	}
}

func TestAnnounceOnUnknownRoomIs404(t *testing.T) {
	srv, _ := newServer(t)
	body := `{"token":"x","host_id":"` + testHostID + `","addrs":["203.0.113.7:5000"]}`
	resp := postJSON(t, srv.URL+"/rooms/AAAAAA/announce", body)

	if resp.StatusCode != http.StatusNotFound {
		t.Fatalf("不存在的房间应当 404，实际 %d", resp.StatusCode)
	}
}

func TestAnnounceRejectsBadCandidates(t *testing.T) {
	srv, _ := newServer(t)
	_, created := postRoom(t, srv.URL, `{}`)

	body := `{"token":"` + created.Token + `","host_id":"` + testHostID + `","addrs":["nope"]}`
	resp := postJSON(t, srv.URL+"/rooms/"+created.Code+"/announce", body)

	if resp.StatusCode != http.StatusBadRequest {
		t.Fatalf("坏候选地址应当 400，实际 %d", resp.StatusCode)
	}
	if code := errorCode(t, resp); code != "bad_candidates" {
		t.Fatalf("error = %q，想要 bad_candidates", code)
	}
}

// 客户端靠 node_id 验节点码里的 pin：配了就必须出现在响应里。
func TestNodeIDIsExposedWhenConfigured(t *testing.T) {
	c := &clock{t: time.Date(2026, 10, 28, 12, 0, 0, 0, time.UTC)}
	svc := rooms.NewService(rooms.NewMemoryStore(), time.Minute).Clock(c.now)
	const nodeID = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"
	srv := newServerWith(t, svc, httpapi.WithNodeID(nodeID))

	_, created := postRoom(t, srv.URL, `{}`)
	if created.NodeID != nodeID {
		t.Fatalf("开门响应 node_id = %q，想要 %q", created.NodeID, nodeID)
	}

	got := decodeRoom(t, get(t, srv.URL+"/rooms/"+created.Code))
	if got.NodeID != nodeID {
		t.Fatalf("查码响应 node_id = %q", got.NodeID)
	}
}
