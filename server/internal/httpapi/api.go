// Package httpapi 是节点的对外 HTTP 接口。
//
// 接口契约：
//
//	POST   /rooms          {port}  → 201 {code, port, created_at, expires_at, ttl_seconds,
//	                                       token, node_id}
//	                                  429 too_many_requests（开门请求刷太快）
//	                                  503 node_full（这个节点的房间满了）
//	POST   /rooms/{code}/announce
//	                       {token, host_id, addrs}
//	                                → 200 {房间 + 房主身份与候选地址}
//	                                  403 bad_token（不是开这个门的人）
//	                                  404 code_not_found
//	                                  400 bad_host_id / bad_candidates
//	                                  409 too_many_announces
//	GET    /rooms/{code}           → 200 {...} | 404 code_not_found | 429 too_many_requests
//	DELETE /rooms/{code}           → 204 | 404 code_not_found | 429 too_many_requests
//	GET    /healthz                → 200 {status:"ok", name:"香港·荃湾", code:"<节点码>", proto:{api,transport}, rooms:N}
//
// 三处刻意的设计：
//
//  1. 404 的两种情况（门牌号形状不对、房间不存在/已过期）**都返回 404 + code_not_found**。
//     对用户是同一句话，客户端也就不必把两种状态码翻译成同一个失败码。
//  2. DELETE 也要走查码限速——它同样会泄露"这个门牌号存不存在"，
//     只限 GET 等于留了另一条枚举通道。
//  3. **令牌只在 201 那一次出现**，而且节点只存它的哈希。查码响应里永远没有令牌，
//     否则拿到门牌号的人就能冒充房主去改候选地址。
package httpapi

import (
	"encoding/json"
	"errors"
	"math"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"strconv"
	"strings"
	"time"

	"knock/internal/ratelimit"
	"knock/internal/rooms"
)

// DefaultPort 是 Minecraft Java 版默认端口。
const DefaultPort = 25565

// maxBodyBytes 限制请求体，避免一个请求吃掉内存。
const maxBodyBytes = 4 << 10

// API 把房间服务暴露成 HTTP。
type API struct {
	svc      *rooms.Service
	lookup   *ratelimit.Limiter
	create   *ratelimit.Limiter
	nodeID   string
	relayURL string
	name     string
	// code 是这个节点的编码，客户端拿它拼完整门牌号。
	code string
}

// Option 是可选的防护配置。不传就是不限速（只在测试或本机调试时这样用）。
type Option func(*API)

// WithLookupLimiter 给查码（GET/DELETE）限速。公网节点必须配。
func WithLookupLimiter(l *ratelimit.Limiter) Option {
	return func(a *API) { a.lookup = l }
}

// WithCreateLimiter 给开门（POST）限速。公网节点必须配。
func WithCreateLimiter(l *ratelimit.Limiter) Option {
	return func(a *API) { a.create = l }
}

// WithNodeID 把节点自己的公钥（32 字节 hex）放进响应。
//
// 客户端拿它算 SHA-256 前 8 字节，和节点码里的 pin 比对——对不上就**拒绝**。
// 这就是"这个接口背后是不是我要找的那台机器"的唯一答案。
func WithNodeID(id string) Option {
	return func(a *API) { a.nodeID = id }
}

// WithRelayURL 告诉客户端这个节点的中继在哪。
//
// 客户端只认这一个中继（`RelayMode::Custom`），**永不**回落到 n0 的公共中继。
// 留空表示这个节点只做信令。
func WithRelayURL(u string) Option {
	return func(a *API) { a.relayURL = u }
}

// WithName 设置节点对外显示的名字，比如「香港·荃湾」。
// 名字由节点自己报，不依赖任何公共名单——名单只是省去手输节点码。
// WithNodeCode 把节点编码挂上去，让 /healthz 能报出来。
//
// 客户端拼门牌号（<节点码>.<房间码>）要用它；不带的话，不在列表里的节点
// 就只能给出 6 位短码，朋友还得先加节点。
func WithNodeCode(code string) Option {
	return func(a *API) { a.code = code }
}

func WithName(name string) Option {
	return func(a *API) { a.name = name }
}

func New(svc *rooms.Service, opts ...Option) *API {
	a := &API{svc: svc}
	for _, opt := range opts {
		if opt != nil {
			opt(a)
		}
	}
	return a
}

// Handler 返回带路由的 http.Handler。
//
// 如果这台机同时跑中继（iroh-relay 只监听环回），可以把话务一起吞进来：
// 走 /rooms、/healthz 的是本服务，其余路径整条转给中继。
// 这样对外**只开一个端口**——玩家那边少一个要解释的东西，
// 节点主人也少一条防火墙规则。
func (a *API) HandlerWithRelay(relayLocal string) http.Handler {
	base := a.Handler()
	if relayLocal == "" {
		return base
	}
	target, err := url.Parse("http://" + relayLocal)
	if err != nil {
		return base
	}
	proxy := httputil.NewSingleHostReverseProxy(target)
	proxy.ErrorHandler = func(w http.ResponseWriter, r *http.Request, e error) {
		http.Error(w, "中继暂时不可用", http.StatusBadGateway)
	}

	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.URL.Path == "/healthz",
			strings.HasPrefix(r.URL.Path, "/rooms"):
			base.ServeHTTP(w, r)
		default:
			// 中继的 /relay、/ping、/probe… 原样交给它
			proxy.ServeHTTP(w, r)
		}
	})
}

func (a *API) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("POST /rooms", a.createRoom)
	mux.HandleFunc("POST /rooms/{code}/announce", a.announce)
	mux.HandleFunc("GET /rooms/{code}", a.getRoom)
	mux.HandleFunc("DELETE /rooms/{code}", a.closeRoom)
	mux.HandleFunc("GET /healthz", a.health)
	return mux
}

type createRequest struct {
	Port int `json:"port"`
}

type announceRequest struct {
	Token  string   `json:"token"`
	HostID string   `json:"host_id"`
	Addrs  []string `json:"addrs"`
}

// 协议版本。节点与客户端各报各的，不匹配就明说，不硬连。
const (
	// ProtoAPI 是 HTTP 接口的版本
	ProtoAPI = 1
	// ProtoTransport 是数据通路的版本（knock/2 才有控制通道）
	ProtoTransport = 2
)

// protoInfo 随响应发出去，让客户端判断"这个节点更不更新"。
type protoInfo struct {
	API       int `json:"api"`
	Transport int `json:"transport"`
}

type roomResponse struct {
	rooms.Room
	TTLSeconds int `json:"ttl_seconds"`
	// NodeID 是节点自己的公钥（hex），客户端用它验节点码里的 pin
	NodeID string `json:"node_id,omitempty"`
	// RelayURL 是这个节点的中继地址；没有就是只做信令
	RelayURL string `json:"relay_url,omitempty"`
	// Token 只在开门那一次出现
	Token string `json:"token,omitempty"`
	// Proto 是这个节点支持的协议版本
	Proto protoInfo `json:"proto"`
}

type errorResponse struct {
	Error   string `json:"error"`
	Message string `json:"message,omitempty"`
}

func (a *API) createRoom(w http.ResponseWriter, r *http.Request) {
	if ok, retry := a.create.Allow(clientKey(r)); !ok {
		writeRateLimited(w, retry)
		return
	}

	var req createRequest
	body := http.MaxBytesReader(w, r.Body, maxBodyBytes)
	if err := json.NewDecoder(body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "bad_request", "请求体不是合法 JSON")
		return
	}
	if req.Port == 0 {
		req.Port = DefaultPort
	}
	if req.Port < 1 || req.Port > 65535 {
		writeError(w, http.StatusBadRequest, "invalid_port", "端口必须在 1..65535 之间")
		return
	}

	token, err := rooms.NewToken()
	if err != nil {
		writeError(w, http.StatusInternalServerError, "internal_error", "开门失败，稍后再试")
		return
	}

	room, err := a.svc.Create(req.Port, rooms.WithToken(token))
	switch {
	case err == nil:
	case errors.Is(err, rooms.ErrNodeFull):
		writeError(w, http.StatusServiceUnavailable, "node_full", "这个节点的房间满了，换一个节点或等门自己关上")
		return
	case errors.Is(err, rooms.ErrInvalidPort):
		writeError(w, http.StatusBadRequest, "invalid_port", "端口必须在 1..65535 之间")
		return
	default:
		writeError(w, http.StatusInternalServerError, "internal_error", "开门失败，稍后再试")
		return
	}

	writeJSON(w, http.StatusCreated, a.roomPayload(room, token))
}

// announce 是房主把自己的 iroh 身份与直连地址放到房间上。
//
// 房客随后查码拿到 host_id + host_addrs，逐个去拨号——这就是"敲门"。
func (a *API) announce(w http.ResponseWriter, r *http.Request) {
	// 和开门共用一个限速器：两者都是"写"
	if ok, retry := a.create.Allow(clientKey(r)); !ok {
		writeRateLimited(w, retry)
		return
	}

	var req announceRequest
	body := http.MaxBytesReader(w, r.Body, maxBodyBytes)
	if err := json.NewDecoder(body).Decode(&req); err != nil {
		writeError(w, http.StatusBadRequest, "bad_request", "请求体不是合法 JSON")
		return
	}

	code := normalizeCode(r.PathValue("code"))
	room, err := a.svc.Announce(code, req.Token, req.HostID, req.Addrs)
	switch {
	case err == nil:
	case errors.Is(err, rooms.ErrNotFound):
		writeError(w, http.StatusNotFound, "code_not_found", "门牌号不对，或者已经过期")
		return
	case errors.Is(err, rooms.ErrBadToken):
		// 403 而不是 404：房间确实在，只是你不是开门的那个人
		writeError(w, http.StatusForbidden, "bad_token", "只有开这个门的人才发布得了地址")
		return
	case errors.Is(err, rooms.ErrTooFrequent):
		writeError(w, http.StatusTooManyRequests, "too_frequent", "发布太频繁，稍等一下")
		return
	case errors.Is(err, rooms.ErrBadHostID):
		writeError(w, http.StatusBadRequest, "bad_host_id", "房主身份必须是 32 字节的 hex")
		return
	case errors.Is(err, rooms.ErrBadCandidates):
		writeError(w, http.StatusBadRequest, "bad_candidates", "候选地址必须是 ip:port，1 到 8 个")
		return
	default:
		writeError(w, http.StatusInternalServerError, "internal_error", "发布地址失败，稍后再试")
		return
	}

	writeJSON(w, http.StatusOK, a.roomPayload(room, ""))
}

// roomPayload 组装响应。token 只在开门那一次传，其余时候是空串。
func (a *API) roomPayload(room rooms.Room, token string) roomResponse {
	return roomResponse{
		Room:       room,
		TTLSeconds: int(room.ExpiresAt.Sub(room.CreatedAt).Seconds()),
		NodeID:     a.nodeID,
		RelayURL:   a.relayURL,
		Token:      token,
		Proto:      protoInfo{API: ProtoAPI, Transport: ProtoTransport},
	}
}

func (a *API) getRoom(w http.ResponseWriter, r *http.Request) {
	if ok, retry := a.lookup.Allow(clientKey(r)); !ok {
		writeRateLimited(w, retry)
		return
	}

	code := normalizeCode(r.PathValue("code"))

	room, err := a.svc.Lookup(code)
	if err != nil {
		writeError(w, http.StatusNotFound, "code_not_found", "门牌号不对，或者已经过期")
		return
	}

	writeJSON(w, http.StatusOK, a.roomPayload(room, ""))
}

func (a *API) closeRoom(w http.ResponseWriter, r *http.Request) {
	// 关门同样能暴露"门牌号存不存在"，所以和查码共用一个限速器
	if ok, retry := a.lookup.Allow(clientKey(r)); !ok {
		writeRateLimited(w, retry)
		return
	}

	code := normalizeCode(r.PathValue("code"))

	if err := a.svc.Close(code); err != nil {
		if errors.Is(err, rooms.ErrNotFound) {
			writeError(w, http.StatusNotFound, "code_not_found", "门牌号不对，或者已经过期")
			return
		}
		writeError(w, http.StatusInternalServerError, "internal_error", "关门失败")
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (a *API) health(w http.ResponseWriter, _ *http.Request) {
	writeJSON(w, http.StatusOK, map[string]any{
		"status":        "ok",
		"name":          a.name,
		"code":          a.code,
		"proto":         protoInfo{API: ProtoAPI, Transport: ProtoTransport},
		"rooms":         a.svc.Len(),
		"lookup_limit":  a.lookup.Limit(),
		"create_limit":  a.create.Limit(),
		"lookup_bucket": a.lookup.Len(),
		// 节点体检用：客户端靠这两项判断"能不能绕路"
		"relay_url":     a.relayURL,
		"relay_allowed": a.relayURL != "",
	})
}

// normalizeCode 把用户可能带进来的空白、小写、以及 O/I/L 的误写统一掉。
// 门牌号不区分大小写，且 Crockford 允许把 0 抄成 O。
func normalizeCode(raw string) string {
	return rooms.NormalizeCode(raw)
}

// clientKey 用连接对端的 IP 当限速键。
//
// 刻意**不看 X-Forwarded-For**：这个头谁都能伪造，信它等于把限速关掉。
// 将来真要放在反代后面，得改成"只信任显式配置的上游"。
func clientKey(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

func writeJSON(w http.ResponseWriter, status int, payload any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(status)
	_ = json.NewEncoder(w).Encode(payload)
}

func writeError(w http.ResponseWriter, status int, code, message string) {
	writeJSON(w, status, errorResponse{Error: code, Message: message})
}

// writeRateLimited 给出 429 并带上 Retry-After——客户端可以据此退避，
// 而不是立刻重试把情况变得更糟。
func writeRateLimited(w http.ResponseWriter, retry time.Duration) {
	seconds := int(math.Ceil(retry.Seconds()))
	if seconds < 1 {
		seconds = 1
	}
	w.Header().Set("Retry-After", strconv.Itoa(seconds))
	writeError(w, http.StatusTooManyRequests, "too_many_requests", "敲得太快了，等一会儿再敲")
}
