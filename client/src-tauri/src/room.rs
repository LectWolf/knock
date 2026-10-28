//! 房主/房客与节点之间的房间接口。
//!
//! 这是"敲门"能成立的前半段：房主在节点上开门、发布自己的 iroh 身份；
//! 房客用门牌号查回来，拿到 `host_id` 与候选地址。
//!
//! **这个模块是安全边界**：`node_id` 必须和节点码里的 pin 对上，
//! 对不上就拒绝（见 [`pin_matches`]）。节点响应的内容本身不可信，
//! 可信的是"我手里的 pin 和它自报的公钥能对上"。

use serde::Deserialize;

/// 本客户端认的协议版本。节点报的版本对不上就明说，不硬连。
pub const SUPPORTED_API: u32 = 1;
/// 数据通路版本：2 才有控制通道（昵称/踢人）
pub const SUPPORTED_TRANSPORT: u32 = 2;

/// 节点自报的协议版本。
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub struct Proto {
    pub api: u32,
    pub transport: u32,
}

impl Proto {
    /// 只有接口版本完全一致、通路版本不低于我们时才认。
    ///
    /// 不做"尽力兼容"：半通比不通更糟——用户会以为是网络问题，而不是版本问题。
    pub fn is_supported(&self) -> bool {
        self.api == SUPPORTED_API && self.transport >= SUPPORTED_TRANSPORT
    }
}

/// 节点返回的房间。字段多的（创建时间、TTL）不解析——用不上的就不建模。
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct RoomInfo {
    pub code: String,
    pub port: u16,
    /// 节点自己的公钥（hex）。客户端用它验节点码里的 pin。
    #[serde(default)]
    pub node_id: Option<String>,
    /// 房主的 iroh EndpointId（hex）。announce 之后才有。
    #[serde(default)]
    pub host_id: Option<String>,
    /// 房主发布的候选直连地址（`ip:port`）。
    #[serde(default)]
    pub host_addrs: Vec<String>,
    /// 这个节点的中继地址。没有就是只做信令。
    #[serde(default)]
    pub relay_url: Option<String>,
    /// 房间令牌：**只在开门那一次返回**，查码时一定没有。
    #[serde(default)]
    pub token: Option<String>,
    /// 节点支持的协议版本。老节点没有这个字段。
    #[serde(default)]
    pub proto: Option<Proto>,
}

/// 房间接口的失败。刻意做成"能翻译成用户那句话"的形状，
/// 而不是把 HTTP 状态码漏到上层。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomError {
    /// 门牌号不对，或者已经过期 → `code_not_found`
    NotFound,
    /// 令牌不对，不是开这个门的人
    BadToken,
    /// 节点房间满了 → 该换节点
    NodeFull,
    /// 敲太快了，带服务端建议的等待秒数
    RateLimited { retry_after: Option<u64> },
    /// 同一房间发布地址次数用完
    TooManyAnnounces,
    /// 请求本身有问题（形状不对）
    BadRequest(String),
    /// 响应不是预期形状
    Protocol(String),
    /// 网络层失败：连不上、超时
    Network(String),
    /// 门开着，但房主还没把自己的地址挂上来（或者挂了却是一条都连不上的路）
    HostNotReady,
    /// 节点版本太旧（或太新），说不到一块去
    NodeOutdated { api: u32 },
    /// 节点的公钥和节点码里的 pin 对不上 —— **这是攻击信号，不是普通错误**
    PinMismatch,
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoomError::NotFound => write!(f, "门牌号不对，或者已经过期"),
            RoomError::BadToken => write!(f, "只有开这个门的人才发布得了地址"),
            RoomError::NodeFull => write!(f, "这个节点的房间满了"),
            RoomError::RateLimited { retry_after } => match retry_after {
                Some(secs) => write!(f, "敲得太快了，等 {secs} 秒再敲"),
                None => write!(f, "敲得太快了，等一会儿再敲"),
            },
            RoomError::TooManyAnnounces => write!(f, "地址发布太多次了，重新开一次门吧"),
            RoomError::BadRequest(msg) => write!(f, "请求不合法：{msg}"),
            RoomError::Protocol(msg) => write!(f, "节点返回的东西看不懂：{msg}"),
            RoomError::Network(msg) => write!(f, "连不上节点：{msg}"),
            RoomError::HostNotReady => write!(f, "门开着，但屋里没人"),
            RoomError::NodeOutdated { api } => {
                write!(f, "这个节点的版本对不上（它报的是 v{api}），让服务器主人更新一下")
            }
            RoomError::PinMismatch => write!(f, "这个节点的身份和门牌上写的不一样，已拒绝"),
        }
    }
}

impl std::error::Error for RoomError {}

/// 房间接口。抽出 trait 是为了让状态机与传输层能拿假的实现测。
pub trait RoomClient {
    /// 房主开门。返回的 `RoomInfo.token` 是唯一一次能拿到令牌的机会。
    fn open(&self, port: u16) -> Result<RoomInfo, RoomError>;
    /// 房主发布自己的 iroh 身份与候选地址。必须带开门时得到的令牌。
    fn announce(
        &self,
        code: &str,
        token: &str,
        host_id: &str,
        addrs: &[String],
    ) -> Result<(), RoomError>;
    /// 房客查码。
    fn lookup(&self, code: &str) -> Result<RoomInfo, RoomError>;
    /// 房主关门。
    fn close(&self, code: &str) -> Result<(), RoomError>;
}

/// 走 HTTP 的真实实现。
///
/// 这一版是明文 HTTP（节点还没上 TLS）；接口形状不变，将来换成 HTTPS 只改 base。
pub struct HttpRoomClient {
    base: String,
}

impl HttpRoomClient {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into() }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base.trim_end_matches('/'), path)
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<(u16, String), RoomError> {
        let url = self.url(path);
        let builder = match method {
            "GET" => attohttpc::get(&url),
            "POST" => attohttpc::post(&url),
            "DELETE" => attohttpc::delete(&url),
            other => return Err(RoomError::Protocol(format!("不支持的请求方法 {other}"))),
        };

        // attohttpc 的 RequestBuilder 对 body 类型是泛型的，带 body / 不带 body
        // 是两种类型，所以分叉要落在 send() 上，不能落在 builder 上。
        let response = match body {
            Some(body) => builder
                .header("Content-Type", "application/json")
                .text(body)
                .send(),
            None => builder.send(),
        }
        .map_err(|e| RoomError::Network(e.to_string()))?;
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let text = response.text().unwrap_or_default();

        if (200..300).contains(&status) {
            return Ok((status, text));
        }

        Err(match status {
            404 => RoomError::NotFound,
            403 => RoomError::BadToken,
            409 => RoomError::TooManyAnnounces,
            429 => RoomError::RateLimited { retry_after },
            503 => RoomError::NodeFull,
            400 => RoomError::BadRequest(text),
            other => RoomError::Protocol(format!("HTTP {other}: {text}")),
        })
    }

    fn parse_room(text: &str) -> Result<RoomInfo, RoomError> {
        serde_json::from_str(text).map_err(|e| RoomError::Protocol(e.to_string()))
    }
}

impl RoomClient for HttpRoomClient {
    fn open(&self, port: u16) -> Result<RoomInfo, RoomError> {
        let body = serde_json::json!({ "port": port }).to_string();
        let (_, text) = self.request("POST", "/rooms", Some(&body))?;
        Self::parse_room(&text)
    }

    fn announce(
        &self,
        code: &str,
        token: &str,
        host_id: &str,
        addrs: &[String],
    ) -> Result<(), RoomError> {
        let body = serde_json::json!({
            "token": token,
            "host_id": host_id,
            "addrs": addrs,
        })
        .to_string();
        let path = format!("/rooms/{code}/announce");
        self.request("POST", &path, Some(&body))?;
        Ok(())
    }

    fn lookup(&self, code: &str) -> Result<RoomInfo, RoomError> {
        let path = format!("/rooms/{code}");
        let (_, text) = self.request("GET", &path, None)?;
        Self::parse_room(&text)
    }

    fn close(&self, code: &str) -> Result<(), RoomError> {
        let path = format!("/rooms/{code}");
        self.request("DELETE", &path, None)?;
        Ok(())
    }
}

/// 用节点码里的 pin 校验节点自报的公钥。
///
/// 对不上的意思是"这个接口背后不是我要找的那台机器"——可能是别人在冒充，
/// 也可能只是节点换了密钥。两种情况都不该继续连，所以返回 false 而不是警告。
pub fn pin_matches(pin: &[u8; 8], node_id_hex: &str) -> bool {
    let Some(raw) = hex_decode(node_id_hex) else {
        return false;
    };
    if raw.len() != 32 {
        return false;
    }
    let digest = <sha2::Sha256 as sha2::Digest>::digest(&raw);
    digest[..8] == pin[..]
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for pair in bytes.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out.push(((hi << 4) | lo) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE_JSON: &str = r#"{
        "code": "A7K2M9",
        "port": 25565,
        "created_at": "2026-10-28T10:00:00Z",
        "expires_at": "2026-10-28T10:10:00Z",
        "ttl_seconds": 600,
        "node_id": "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
        "host_id": "abababababababababababababababababababababababababababababababab",
        "host_addrs": ["203.0.113.7:5000"]
    }"#;

    #[test]
    fn parses_the_nodes_actual_json_shape() {
        let room: RoomInfo = serde_json::from_str(NODE_JSON).expect("能解析节点的真实响应");
        assert_eq!(room.code, "A7K2M9");
        assert_eq!(room.port, 25565);
        assert_eq!(room.host_addrs, vec!["203.0.113.7:5000"]);
        assert!(room.token.is_none(), "查码响应里不该有令牌");
        assert_eq!(room.node_id.as_deref().map(str::len), Some(64));
    }

    #[test]
    fn pin_from_the_same_key_matches() {
        let key = [0xab_u8; 32];
        let node_id = "ab".repeat(32);
        let digest = <sha2::Sha256 as sha2::Digest>::digest(key);
        let mut pin = [0u8; 8];
        pin.copy_from_slice(&digest[..8]);

        assert!(pin_matches(&pin, &node_id), "同一个公钥必须对得上");
    }

    #[test]
    fn a_different_key_never_matches() {
        let node_id = "ab".repeat(32);
        let wrong_pin = [0u8; 8];
        assert!(!pin_matches(&wrong_pin, &node_id), "全零 pin 不该对得上");
    }

    #[test]
    fn a_single_changed_byte_breaks_the_pin() {
        let digest = <sha2::Sha256 as sha2::Digest>::digest([0xab_u8; 32]);
        let mut pin = [0u8; 8];
        pin.copy_from_slice(&digest[..8]);

        let mut tampered = [0xab_u8; 32];
        tampered[7] ^= 0x01; // 改一个字节
        let tampered_hex: String = tampered.iter().map(|b| format!("{b:02x}")).collect();

        assert!(!pin_matches(&pin, &tampered_hex), "改一字节就必须对不上");
    }

    #[test]
    fn malformed_node_ids_are_rejected() {
        let pin = [0u8; 8];
        assert!(!pin_matches(&pin, ""), "空串");
        assert!(!pin_matches(&pin, "zz"), "非 hex");
        assert!(!pin_matches(&pin, "abcd"), "长度不对");
        assert!(
            !pin_matches(&pin, &"ab".repeat(33)),
            "33 字节也必须拒绝，不能只查前缀"
        );
    }

    #[test]
    fn a_current_node_is_accepted() {
        let json = r#"{"code":"A7K2M9","port":25565,"proto":{"api":1,"transport":2}}"#;
        let room: RoomInfo = serde_json::from_str(json).unwrap();
        assert!(room.proto.expect("应当有版本").is_supported());
    }

    #[test]
    fn an_outdated_node_is_refused() {
        // 只报 transport 1：没有控制通道，昵称和踢人都没处放
        let old = Proto { api: 1, transport: 1 };
        assert!(!old.is_supported());
        // 接口版本不一致也拒
        let future = Proto { api: 2, transport: 2 };
        assert!(!future.is_supported());
    }

    #[test]
    fn a_node_without_version_is_treated_as_old() {
        let json = r#"{"code":"A7K2M9","port":25565}"#;
        let room: RoomInfo = serde_json::from_str(json).unwrap();
        assert!(room.proto.is_none(), "老节点不报版本，由上层当老版本处理");
    }

    #[test]
    fn relay_url_is_optional() {
        let with = r#"{"code":"A7K2M9","port":25565,"relay_url":"https://relay.example.com"}"#;
        let room: RoomInfo = serde_json::from_str(with).expect("带 relay_url 要能解析");
        assert_eq!(room.relay_url.as_deref(), Some("https://relay.example.com"));

        let without = r#"{"code":"A7K2M9","port":25565}"#;
        let room: RoomInfo = serde_json::from_str(without).expect("不带 relay_url 也要能解析");
        assert!(room.relay_url.is_none(), "没有中继 = 只做信令");
    }

    #[test]
    fn urls_are_joined_without_doubling_slashes() {
        let client = HttpRoomClient::new("http://127.0.0.1:8443/");
        assert_eq!(client.url("/rooms"), "http://127.0.0.1:8443/rooms");
        let client = HttpRoomClient::new("http://127.0.0.1:8443");
        assert_eq!(client.url("/rooms"), "http://127.0.0.1:8443/rooms");
    }
}
