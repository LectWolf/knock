//! 敲门前的最后一步：**查码 → 验身份 → 拿到可拨的目标**。
//!
//! 这一层刻意不碰 iroh 类型，只产出纯数据。
//! 好处是它能在没有网络的情况下把"该不该连"这件事测干净：
//! pin 对不上、房主还没挂地址、地址形状不对——都在这里被拦住。

use crate::room::{pin_matches, RoomClient, RoomError};

/// 敲门敲到的那个目标。等宽：只有真能用的东西才会出现在这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTarget {
    /// 门牌号（规范形式，6 位大写）
    pub code: String,
    /// 房主的 EndpointId，64 个 hex 字符
    pub host_id_hex: String,
    /// 房主发布的候选直连地址（`ip:port`），可能为空——那就只能走中继
    pub addrs: Vec<String>,
    /// 房主所在的中继。有了它"绕路"才有意义
    pub relay_url: Option<String>,
}

impl HostTarget {
    /// 有没有直连地址可试。没有就只能靠中继。
    pub fn has_direct(&self) -> bool {
        !self.addrs.is_empty()
    }

    /// 完全没有路可走：既没有直连地址，也没有中继。
    pub fn is_unreachable(&self) -> bool {
        !self.has_direct() && self.relay_url.is_none()
    }
}

/// 查码、验 pin、组装目标。
///
/// `pin` 来自用户手里的节点码。这是唯一能把"这个接口"和"我要找的那台机器"
/// 绑在一起的东西——对不上就不连，不降级、不警告。
pub fn resolve(client: &dyn RoomClient, code: &str, pin: &[u8; 8]) -> Result<HostTarget, RoomError> {
    let room = client.lookup(code)?;

    let node_id = room
        .node_id
        .as_deref()
        .ok_or_else(|| RoomError::Protocol("响应里没有 node_id，没法验身份".to_string()))?;

    if !pin_matches(pin, node_id) {
        return Err(RoomError::PinMismatch);
    }

    // 版本对不上就别往下走了：硬连上去只会出现"说不到一块"的怪现象
    match room.proto {
        Some(proto) if proto.is_supported() => {}
        other => {
            return Err(RoomError::NodeOutdated {
                api: other.map(|p| p.api).unwrap_or(0),
            })
        }
    }

    let host_id_hex = room
        .host_id
        .ok_or(RoomError::HostNotReady)?
        .to_ascii_lowercase();

    // 形状先过一遍：32 字节 hex，和节点侧校验同一套规则
    if host_id_hex.len() != 64 || !host_id_hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RoomError::Protocol(format!("房主身份形状不对: {host_id_hex}")));
    }

    let target = HostTarget {
        code: room.code,
        host_id_hex,
        addrs: room.host_addrs,
        relay_url: room.relay_url.filter(|u| !u.trim().is_empty()),
    };

    if target.is_unreachable() {
        // 房主 announce 了身份却没给任何可用地址：门开着，但屋里没人
        return Err(RoomError::HostNotReady);
    }

    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::RoomInfo;

    struct FakeClient {
        room: Result<RoomInfo, RoomError>,
    }

    impl RoomClient for FakeClient {
        fn open(&self, _port: u16) -> Result<RoomInfo, RoomError> {
            unreachable!("敲门时不会开门")
        }
        fn announce(&self, _c: &str, _t: &str, _h: &str, _a: &[String]) -> Result<(), RoomError> {
            unreachable!("房客不 announce")
        }
        fn lookup(&self, _code: &str) -> Result<RoomInfo, RoomError> {
            self.room.clone()
        }
        fn close(&self, _code: &str) -> Result<(), RoomError> {
            unreachable!("房客不关门")
        }
    }

    const NODE_ID: &str = "abababababababababababababababababababababababababababababababab";
    const HOST_ID: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    fn pin_for(node_id_hex: &str) -> [u8; 8] {
        // 用现成的 hex 解码路径算 pin：复用 room 模块的行为
        let mut pin = [0u8; 8];
        // pin_matches 内部会算，这里反向取一段固定值不合用；
        // 所以直接构造一个已知的 pin：对 0xab*32 求 SHA-256 前 8 字节。
        let digest = <sha2::Sha256 as sha2::Digest>::digest(
            (0..32)
                .map(|i| u8::from_str_radix(&node_id_hex[i * 2..i * 2 + 2], 16).unwrap())
                .collect::<Vec<_>>(),
        );
        pin.copy_from_slice(&digest[..8]);
        pin
    }

    fn room(host_id: Option<&str>, addrs: &[&str], relay: Option<&str>) -> RoomInfo {
        RoomInfo {
            code: "A7K2M9".into(),
            port: 25565,
            node_id: Some(NODE_ID.into()),
            host_id: host_id.map(Into::into),
            host_addrs: addrs.iter().map(|s| s.to_string()).collect(),
            relay_url: relay.map(Into::into),
            token: None,
            proto: Some(crate::room::Proto { api: 1, transport: 2 }),
        }
    }

    #[test]
    fn resolves_a_direct_target() {
        let client = FakeClient { room: Ok(room(Some(HOST_ID), &["203.0.113.7:5000"], None)) };
        let target = resolve(&client, "A7K2M9", &pin_for(NODE_ID)).expect("应当能解析");
        assert_eq!(target.host_id_hex, HOST_ID);
        assert!(target.has_direct());
        assert!(!target.is_unreachable());
    }

    #[test]
    fn a_wrong_pin_stops_everything() {
        let client = FakeClient { room: Ok(room(Some(HOST_ID), &["203.0.113.7:5000"], None)) };
        let mut wrong = pin_for(NODE_ID);
        wrong[0] ^= 0xff;
        assert_eq!(resolve(&client, "A7K2M9", &wrong).unwrap_err(), RoomError::PinMismatch);
    }

    #[test]
    fn host_that_has_not_announced_is_not_ready() {
        let client = FakeClient { room: Ok(room(None, &[], None)) };
        assert_eq!(resolve(&client, "A7K2M9", &pin_for(NODE_ID)).unwrap_err(), RoomError::HostNotReady);
    }

    #[test]
    fn relay_only_host_is_still_reachable() {
        // 打洞失败、只挂中继的房主：有路，但不是直连
        let client = FakeClient {
            room: Ok(room(Some(HOST_ID), &[], Some("https://relay.example.com"))),
        };
        let target = resolve(&client, "A7K2M9", &pin_for(NODE_ID)).expect("有中继就能连");
        assert!(!target.has_direct());
        assert!(!target.is_unreachable());
    }

    #[test]
    fn no_address_and_no_relay_is_unreachable() {
        let client = FakeClient { room: Ok(room(Some(HOST_ID), &[], None)) };
        assert_eq!(resolve(&client, "A7K2M9", &pin_for(NODE_ID)).unwrap_err(), RoomError::HostNotReady);
    }

    #[test]
    fn a_malformed_host_id_is_rejected() {
        let client = FakeClient { room: Ok(room(Some("not-hex"), &["203.0.113.7:5000"], None)) };
        assert!(matches!(
            resolve(&client, "A7K2M9", &pin_for(NODE_ID)),
            Err(RoomError::Protocol(_))
        ));
    }

    #[test]
    fn missing_node_id_is_a_protocol_error() {
        let mut r = room(Some(HOST_ID), &["203.0.113.7:5000"], None);
        r.node_id = None;
        let client = FakeClient { room: Ok(r) };
        assert!(matches!(
            resolve(&client, "A7K2M9", &pin_for(NODE_ID)),
            Err(RoomError::Protocol(_))
        ));
    }
}
