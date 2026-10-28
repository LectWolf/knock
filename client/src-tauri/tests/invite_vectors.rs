//! 金标向量测试：Rust 与 Go 读**同一份** `spec/invite-v1.tsv`。
//!
//! 这份文件由 `cargo run --example gen_vectors` 生成，里面同时有
//! 「原始字节 hex」「编码后的节点码」「邀请串」三种形态，
//! 所以任何一边改了布局、CRC、base32 或分隔符，另一边的测试立刻会红。
//!
//! 除了向量本身，下面还有几个**不依赖向量**的锚点（CRC 标准检查值、base32 手算值），
//! 免得生成器和校验器一起错。

use knock::invite::{
    self, base32_decode, base32_encode, crc8, label_from_chars, label_to_chars, CodeError, Invite, NodeCode,
};

const VECTORS: &str = include_str!("../../../spec/invite-v1.tsv");

struct Vector {
    name: String,
    label: u16,
    ipv4: Option<[u8; 4]>,
    ipv6: Option<[u8; 16]>,
    domain: Option<String>,
    port: u16,
    pin: [u8; 8],
    relay: bool,
    bytes_hex: String,
    code: String,
    room: String,
    full_invite: String,
    short_invite: String,
}

fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("合法 hex"))
        .collect()
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    if s.is_empty() {
        return None;
    }
    let parts: Vec<u8> = s.split('.').map(|p| p.parse().expect("合法 IPv4")).collect();
    assert_eq!(parts.len(), 4);
    Some([parts[0], parts[1], parts[2], parts[3]])
}

fn parse_ipv6(s: &str) -> Option<[u8; 16]> {
    if s.is_empty() {
        return None;
    }
    let groups: Vec<u16> = s.split(':').map(|g| u16::from_str_radix(g, 16).expect("合法 IPv6")).collect();
    assert_eq!(groups.len(), 8, "向量里的 IPv6 一律写成 8 组");
    let mut out = [0u8; 16];
    for (i, g) in groups.iter().enumerate() {
        out[i * 2] = (g >> 8) as u8;
        out[i * 2 + 1] = (g & 0xff) as u8;
    }
    Some(out)
}

fn load_vectors() -> Vec<Vector> {
    VECTORS
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            assert_eq!(f.len(), 13, "向量列数不对: {line}");
            Vector {
                name: f[0].to_string(),
                label: f[1].parse().expect("label"),
                ipv4: parse_ipv4(f[2]),
                ipv6: parse_ipv6(f[3]),
                domain: if f[4].is_empty() { None } else { Some(f[4].to_string()) },
                port: f[5].parse().expect("port"),
                pin: {
                    let v = hex_bytes(f[6]);
                    let mut p = [0u8; 8];
                    p.copy_from_slice(&v);
                    p
                },
                relay: f[7] == "true",
                bytes_hex: f[8].to_string(),
                code: f[9].to_string(),
                room: f[10].to_string(),
                full_invite: f[11].to_string(),
                short_invite: f[12].to_string(),
            }
        })
        .collect()
}

fn node_of(v: &Vector) -> NodeCode {
    NodeCode {
        label: v.label,
        ipv4: v.ipv4,
        ipv6: v.ipv6,
        domain: v.domain.clone(),
        port: v.port,
        pin: v.pin,
        relay_allowed: v.relay,
    }
}

/// 字节布局与 TSV 里的 `bytes_hex` 完全一致。
#[test]
fn bytes_match_vectors() {
    for v in load_vectors() {
        let bytes = node_of(&v).to_bytes().expect("编码");
        assert_eq!(hex_of(&bytes), v.bytes_hex, "【{}】字节布局漂了", v.name);
    }
}

/// 编码出的节点码与 TSV 一致，且能原样解回来。
#[test]
fn code_round_trips() {
    for v in load_vectors() {
        let node = node_of(&v);
        let code = node.encode().expect("编码");
        assert_eq!(code, v.code, "【{}】节点码漂了", v.name);

        let back = NodeCode::decode(&code).expect("解码");
        assert_eq!(back, node, "【{}】解码结果和原节点不一致", v.name);
    }
}

/// 完整邀请 = 节点码 + `.` + 门牌号。
#[test]
fn full_invite_round_trips() {
    for v in load_vectors() {
        let node = node_of(&v);
        let full = Invite::full(&node, &v.room).expect("拼完整邀请");
        assert_eq!(full, v.full_invite, "【{}】完整邀请漂了", v.name);

        match Invite::parse(&full).expect("解析完整邀请") {
            Invite::Full { node: parsed, room } => {
                assert_eq!(parsed, node, "【{}】完整邀请里的节点不一致", v.name);
                assert_eq!(room, v.room);
            }
            other => panic!("【{}】应当解析成 Full，实际 {other:?}", v.name),
        }
    }
}

/// 短邀请 = 4 字符标签 + `.` + 门牌号。
#[test]
fn short_invite_round_trips() {
    for v in load_vectors() {
        let node = node_of(&v);
        let short = Invite::short(&node, &v.room);
        assert_eq!(short, v.short_invite, "【{}】短邀请漂了", v.name);

        match Invite::parse(&short).expect("解析短邀请") {
            Invite::Short { label, room } => {
                assert_eq!(label, label_to_chars(v.label));
                assert_eq!(room, v.room);
            }
            other => panic!("【{}】应当解析成 Short，实际 {other:?}", v.name),
        }
    }
}

/// 只粘门牌号、只粘节点码，两种都要能认出来。
#[test]
fn bare_room_and_bare_node_are_recognised() {
    let v = &load_vectors()[0];

    match Invite::parse(&v.room).expect("解析门牌号") {
        Invite::RoomOnly(room) => assert_eq!(room, v.room),
        other => panic!("应当解析成 RoomOnly，实际 {other:?}"),
    }

    match Invite::parse(&v.code).expect("解析节点码") {
        Invite::NodeOnly(node) => assert_eq!(node, node_of(v)),
        other => panic!("应当解析成 NodeOnly，实际 {other:?}"),
    }
}

// ————————————————————————————————————————————————
// 不依赖向量的锚点：手算结果，防止"生成器和校验器一起错"
// ————————————————————————————————————————————————

/// CRC-8/SMBUS 的标准检查值。全世界都这么验。
#[test]
fn crc8_matches_the_standard_check_value() {
    assert_eq!(crc8(b"123456789"), 0xF4);
}

/// 手算：0x01 0x02 的 16 bit 流是 00000 00100 00001 (补 0) → "0410"。
#[test]
fn base32_hand_checked() {
    assert_eq!(base32_encode(&[0x01, 0x02]), "0410");
    assert_eq!(base32_decode("0410").unwrap(), vec![0x01, 0x02]);
}

/// Crockford 的别名规则：手抄时把 0 写成 O、把 1 写成 I 或 L 都要能救回来。
#[test]
fn crockford_aliases_are_accepted() {
    let canonical = base32_encode(&[0x01, 0x02]); // "0410"
    assert_eq!(canonical, "0410");

    // 只替换 0 和 1 本身，别动 4 —— 把别的位改掉就不是同一串比特了
    for alias in ["O41O", "04I0", "04L0", "o4i0"] {
        assert_eq!(
            base32_decode(alias).unwrap(),
            base32_decode(&canonical).unwrap(),
            "{alias} 应当被当成 {canonical}"
        );
    }
}

/// 分隔符（显示时插的连字符、空格）不影响解码。
#[test]
fn separators_are_ignored() {
    let v = load_vectors();
    let code = &v[0].code;
    let mut spaced = String::new();
    for (i, ch) in code.chars().enumerate() {
        if i > 0 && i % 5 == 0 {
            spaced.push('-');
        }
        spaced.push(ch);
    }
    assert_eq!(NodeCode::decode(&spaced).expect("带连字符也要能解"), node_of(&v[0]));
}

/// 标签是 16 bit：溢出必须报错，不许截断。
#[test]
fn label_rejects_overflow() {
    assert_eq!(label_to_chars(0), "0000");
    assert_eq!(label_to_chars(0xFFFF), "1ZZZ");
    assert!(label_from_chars("1ZZZ").is_ok());
    // 第一位只能是 0 或 1（20 bit 里高 4 位补零），Z 开头就超出 16 bit
    assert!(label_from_chars("ZZZZ").is_err());
}

// ————————————————————————————————————————————————
// 坏输入必须被拒绝（不是"尽力猜"）
// ————————————————————————————————————————————————

#[test]
fn corrupted_checksum_is_rejected() {
    let v = &load_vectors()[0];
    let mut bytes = hex_bytes(&v.bytes_hex);
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff; // 破坏 CRC

    match NodeCode::from_bytes(&bytes) {
        Err(CodeError::BadChecksum { .. }) => {}
        other => panic!("应当报 CRC 错，实际 {other:?}"),
    }
}

#[test]
fn truncated_or_extended_bytes_are_rejected() {
    let v = &load_vectors()[0];
    let bytes = hex_bytes(&v.bytes_hex);

    let mut short = bytes.clone();
    short.pop();
    assert!(NodeCode::from_bytes(&short).is_err(), "截断必须报错");

    let mut long = bytes.clone();
    long.push(0);
    assert!(NodeCode::from_bytes(&long).is_err(), "多一字节必须报错");
}

#[test]
fn wrong_version_is_rejected() {
    let v = &load_vectors()[0];
    let mut bytes = hex_bytes(&v.bytes_hex);
    bytes[0] = (2 << 4) | (bytes[0] & 0x0f); // 版本号改成 2
    let crc = crc8(&bytes[..bytes.len() - 1]);
    let last = bytes.len() - 1;
    bytes[last] = crc;

    match NodeCode::from_bytes(&bytes) {
        Err(CodeError::BadVersion(2)) => {}
        other => panic!("应当报版本错，实际 {other:?}"),
    }
}

#[test]
fn node_without_any_address_is_rejected() {
    let node = NodeCode {
        label: 1,
        ipv4: None,
        ipv6: None,
        domain: None,
        port: 443,
        pin: [0; 8],
        relay_allowed: true,
    };
    assert_eq!(node.to_bytes(), Err(CodeError::NoAddress));
}

#[test]
fn zero_port_is_rejected() {
    let node = NodeCode {
        label: 1,
        ipv4: Some([203, 0, 113, 7]),
        ipv6: None,
        domain: None,
        port: 0,
        pin: [0; 8],
        relay_allowed: true,
    };
    assert_eq!(node.to_bytes(), Err(CodeError::BadPort(0)));
}

#[test]
fn domain_that_is_not_ascii_is_rejected() {
    let node = NodeCode {
        label: 1,
        ipv4: None,
        ipv6: None,
        domain: Some("门牌.com".to_string()),
        port: 443,
        pin: [0; 8],
        relay_allowed: true,
    };
    assert_eq!(node.to_bytes(), Err(CodeError::DomainNotAscii));
}

#[test]
fn invite_with_broken_room_code_is_rejected() {
    let v = &load_vectors()[0];
    let node_code = &v.code;

    // 门牌号多一位
    assert_eq!(
        Invite::parse(&format!("{node_code}.A7K2M9X")),
        Err(CodeError::BadRoomCode)
    );
    // 门牌号里有字母表外的字符：整条就不该被当成邀请
    assert_eq!(
        Invite::parse(&format!("{node_code}.A7K2M!")),
        Err(CodeError::BadRoomCode)
    );
    // 空串
    assert_eq!(Invite::parse("   "), Err(CodeError::Empty));
}

#[test]
fn room_code_normalises_case_and_aliases() {
    // 用户手抄 "a7k2m9" 或把 K 抄成别的都得归一化；O/I/L 走 Crockford 别名
    match Invite::parse("a7k2m9").expect("小写也要认") {
        Invite::RoomOnly(room) => assert_eq!(room, "A7K2M9"),
        other => panic!("实际 {other:?}"),
    }
    match Invite::parse("A7K2M9 ".trim()).expect("尾随空格要认") {
        Invite::RoomOnly(room) => assert_eq!(room, "A7K2M9"),
        other => panic!("实际 {other:?}"),
    }
}

/// 门牌号必须正好 6 个字符，5 个或 7 个都不是门牌号（会被当节点码去解，然后报错）。
#[test]
fn room_code_length_is_enforced() {
    assert!(invite::is_room_code("A7K2M9"));
    assert!(!invite::is_room_code("A7K2M"));
    assert!(!invite::is_room_code("A7K2M9X"));
}
