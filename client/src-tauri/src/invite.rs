//! 节点码与邀请串的编解码（ADR-0002 的落地实现）。
//!
//! 格式的**唯一真相是 `spec/invite-v1.tsv`**：那份文件由 Rust 生成，Go 也读同一份，
//! 两边对不上测试就红。所以这个文件不是"尽量符合文档"，而是"必须和 Go 一致"。
//!
//! 布局 v1（多字节整数**小端**）：
//!
//! ```text
//! 0        1         3          3+addr       +2      +8    +1
//! +--------+---------+----------+-----------+-------+-----+-----+
//! |ver|flag|  label  | 地址们…  |    port   |  pin  | crc |
//! +--------+---------+----------+-----------+-------+-----+-----+
//! ```
//!
//! - `ver` = 高 4 bit，当前为 1；`flag` = 低 4 bit（见 `FLAG_*`）
//! - 地址按固定顺序出现：IPv4(4) → IPv6(16) → 域名(1 字节长度 + ASCII)
//! - `crc` 是 CRC-8（poly 0x07 / init 0x00，标准检查值见测试）
//! - 整体用 Crockford Base32 编码，去掉分隔符后即规范形式；解码会忽略 `-`、`_` 和空白，
//!   并把 `O→0`、`I/L→1` 当别名（这是 Crockford 的规矩，也是人能安心手抄的原因）

/// Crockford Base32：没有 I、L、O、U。
pub const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub const VERSION: u8 = 1;

/// 节点公钥指纹长度（SHA-256 截断）。64 bit 足够挡住伪造，又省掉 22 个字符。
pub const PIN_LEN: usize = 8;

/// 门牌号长度。
pub const ROOM_LEN: usize = 6;

/// 短邀请里标签的字符数。
pub const LABEL_CHARS: usize = 4;

pub const FLAG_IPV4: u8 = 1 << 0;
pub const FLAG_IPV6: u8 = 1 << 1;
pub const FLAG_DOMAIN: u8 = 1 << 2;
pub const FLAG_RELAY: u8 = 1 << 3;

/// 域名上限按 DNS 的 253 字节。
pub const MAX_DOMAIN_LEN: usize = 253;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    Empty,
    BadChar(char),
    /// 字节数不对：多了、少了、或尾部有残渣
    BadLength,
    /// base32 尾部填充位不是 0（说明串被截断或拼接过）
    BadPadding,
    BadVersion(u8),
    NoAddress,
    DomainEmpty,
    DomainTooLong(usize),
    DomainNotAscii,
    BadPort(u16),
    BadChecksum { want: u8, got: u8 },
    BadRoomCode,
}

/// 一个节点的自描述编码。**不含门牌号**——门牌号属于房间，不属于节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeCode {
    pub label: u16,
    pub ipv4: Option<[u8; 4]>,
    pub ipv6: Option<[u8; 16]>,
    pub domain: Option<String>,
    pub port: u16,
    pub pin: [u8; PIN_LEN],
    pub relay_allowed: bool,
}

impl NodeCode {
    pub fn flags(&self) -> u8 {
        let mut f = 0u8;
        if self.ipv4.is_some() {
            f |= FLAG_IPV4;
        }
        if self.ipv6.is_some() {
            f |= FLAG_IPV6;
        }
        if self.domain.is_some() {
            f |= FLAG_DOMAIN;
        }
        if self.relay_allowed {
            f |= FLAG_RELAY;
        }
        f
    }

    /// 编成字节串（含末尾 CRC）。
    pub fn to_bytes(&self) -> Result<Vec<u8>, CodeError> {
        if self.ipv4.is_none() && self.ipv6.is_none() && self.domain.is_none() {
            return Err(CodeError::NoAddress);
        }
        if self.port == 0 {
            return Err(CodeError::BadPort(self.port));
        }

        let mut out = Vec::with_capacity(40);
        out.push((VERSION << 4) | self.flags());
        out.extend_from_slice(&self.label.to_le_bytes());
        if let Some(v4) = self.ipv4 {
            out.extend_from_slice(&v4);
        }
        if let Some(v6) = self.ipv6 {
            out.extend_from_slice(&v6);
        }
        if let Some(domain) = &self.domain {
            if domain.is_empty() {
                return Err(CodeError::DomainEmpty);
            }
            if !domain.is_ascii() {
                return Err(CodeError::DomainNotAscii);
            }
            if domain.len() > MAX_DOMAIN_LEN {
                return Err(CodeError::DomainTooLong(domain.len()));
            }
            out.push(domain.len() as u8);
            out.extend_from_slice(domain.as_bytes());
        }
        out.extend_from_slice(&self.port.to_le_bytes());
        out.extend_from_slice(&self.pin);

        let crc = crc8(&out);
        out.push(crc);
        Ok(out)
    }

    /// 从字节串还原。**任何一字节不对都必须报错，不许"尽力猜"。**
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CodeError> {
        let mut cur = Cursor::new(bytes);

        let head = cur.take(1)?[0];
        let version = head >> 4;
        if version != VERSION {
            return Err(CodeError::BadVersion(version));
        }
        let flags = head & 0x0f;
        if flags & (FLAG_IPV4 | FLAG_IPV6 | FLAG_DOMAIN) == 0 {
            return Err(CodeError::NoAddress);
        }

        let label = u16::from_le_bytes(cur.take(2)?.try_into().expect("2 字节"));

        let ipv4 = if flags & FLAG_IPV4 != 0 {
            let raw = cur.take(4)?;
            Some([raw[0], raw[1], raw[2], raw[3]])
        } else {
            None
        };

        let ipv6 = if flags & FLAG_IPV6 != 0 {
            let mut a = [0u8; 16];
            a.copy_from_slice(cur.take(16)?);
            Some(a)
        } else {
            None
        };

        let domain = if flags & FLAG_DOMAIN != 0 {
            let len = cur.take(1)?[0] as usize;
            if len == 0 {
                return Err(CodeError::DomainEmpty);
            }
            let raw = cur.take(len)?;
            Some(String::from_utf8(raw.to_vec()).map_err(|_| CodeError::DomainNotAscii)?)
        } else {
            None
        };

        let port = u16::from_le_bytes(cur.take(2)?.try_into().expect("2 字节"));
        if port == 0 {
            return Err(CodeError::BadPort(port));
        }

        let mut pin = [0u8; PIN_LEN];
        pin.copy_from_slice(cur.take(PIN_LEN)?);

        let want = cur.take(1)?[0];
        let got = crc8(&bytes[..cur.pos - 1]);
        if want != got {
            return Err(CodeError::BadChecksum { want, got });
        }
        if cur.pos != bytes.len() {
            return Err(CodeError::BadLength);
        }

        Ok(NodeCode { label, ipv4, ipv6, domain, port, pin, relay_allowed: flags & FLAG_RELAY != 0 })
    }

    /// 规范形式：Crockford Base32，大写、无分隔符。
    pub fn encode(&self) -> Result<String, CodeError> {
        Ok(base32_encode(&self.to_bytes()?))
    }

    pub fn decode(input: &str) -> Result<Self, CodeError> {
        Self::from_bytes(&base32_decode(input)?)
    }

    /// 短邀请里那 4 个字符。
    pub fn label_chars(&self) -> String {
        label_to_chars(self.label)
    }
}

/// 用户粘贴进来的东西。四种形态，解析只有这一处。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invite {
    /// 只给了 6 位门牌号
    RoomOnly(String),
    /// `K7F2.A7K2M9`
    Short { label: String, room: String },
    /// `<节点码>.A7K2M9`
    Full { node: NodeCode, room: String },
    /// 只给了节点码 = 添加这个节点，不是敲门
    NodeOnly(NodeCode),
}

impl Invite {
    /// 解析用户粘贴的任意一串东西。
    pub fn parse(input: &str) -> Result<Self, CodeError> {
        let cleaned = sanitize(input);
        if cleaned.is_empty() {
            return Err(CodeError::Empty);
        }

        let Some((left, right)) = cleaned.split_once('.') else {
            // 没有分隔符：要么是光秃秃的门牌号，要么是光秃秃的节点码
            return if is_room_code(&cleaned) {
                Ok(Invite::RoomOnly(normalize_room(&cleaned)))
            } else {
                Ok(Invite::NodeOnly(NodeCode::decode(&cleaned)?))
            };
        };

        if !is_room_code(right) {
            return Err(CodeError::BadRoomCode);
        }
        let room = normalize_room(right);

        // 左边 4 个字符只可能是标签；节点码最短也有 29 个字符，不会撞
        if left.chars().count() == LABEL_CHARS {
            Ok(Invite::Short { label: left.to_uppercase(), room })
        } else {
            Ok(Invite::Full { node: NodeCode::decode(left)?, room })
        }
    }

    pub fn room(&self) -> Option<&str> {
        match self {
            Invite::RoomOnly(room) => Some(room),
            Invite::Short { room, .. } => Some(room),
            Invite::Full { room, .. } => Some(room),
            Invite::NodeOnly(_) => None,
        }
    }

    pub fn node(&self) -> Option<&NodeCode> {
        match self {
            Invite::Full { node, .. } => Some(node),
            Invite::NodeOnly(node) => Some(node),
            _ => None,
        }
    }

    /// 完整邀请：`<节点码>.<门牌号>`
    pub fn full(node: &NodeCode, room: &str) -> Result<String, CodeError> {
        Ok(format!("{}.{}", node.encode()?, normalize_room(room)))
    }

    /// 短邀请：`<标签>.<门牌号>`
    pub fn short(node: &NodeCode, room: &str) -> String {
        format!("{}.{}", node.label_chars(), normalize_room(room))
    }
}

/// 房主该发给朋友的那一串。
///
/// 门牌号就是完整邀请；节点码解析不出来时退回 6 位房间码——
/// 宁可给一串短的（对方得自己加节点），也别给一串错的。
pub fn door_label(node_code: &str, room: &str) -> String {
    match NodeCode::decode(node_code.trim()) {
        Ok(node) => Invite::full(&node, room).unwrap_or_else(|_| normalize_room(room)),
        Err(_) => normalize_room(room),
    }
}

impl std::fmt::Display for Invite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Invite::RoomOnly(room) => write!(f, "{room}"),
            Invite::Short { label, room } => write!(f, "{label}.{room}"),
            Invite::Full { node, room } => write!(f, "{}.{}", node.encode().unwrap_or_default(), room),
            Invite::NodeOnly(node) => write!(f, "{}", node.encode().unwrap_or_default()),
        }
    }
}

// ————————————————————————————————————————————————
// 编码原语
// ————————————————————————————————————————————————

/// CRC-8/SMBUS：poly 0x07、init 0x00、不反射、xorout 0。
pub fn crc8(data: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &byte in data {
        crc ^= byte;
        for _ in 0..8 {
            if crc & 0x80 != 0 {
                crc = (crc << 1) ^ 0x07;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

/// Crockford Base32 编码（尾部补 0 到整字符）。
pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 8 / 5 + 2);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;

    for &byte in data {
        acc = (acc << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((acc >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (5 - bits)) & 0x1f) as usize] as char);
    }
    out
}

/// Crockford Base32 解码。忽略 `-`、`_` 和空白；`O→0`、`I/L→1`。
pub fn base32_decode(input: &str) -> Result<Vec<u8>, CodeError> {
    let mut out = Vec::with_capacity(input.len() * 5 / 8 + 1);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;

    for ch in input.chars() {
        if ch == '-' || ch == '_' || ch.is_whitespace() {
            continue;
        }
        let value = decode_char(ch)?;
        acc = (acc << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }

    // 规范编码的尾巴最多剩 4 个"补出来的 0"；多了说明串被拼接过，少了说明被截断
    if bits >= 5 {
        return Err(CodeError::BadLength);
    }
    if bits > 0 && acc & ((1 << bits) - 1) != 0 {
        return Err(CodeError::BadPadding);
    }
    if out.is_empty() {
        return Err(CodeError::Empty);
    }
    Ok(out)
}

fn decode_char(ch: char) -> Result<u8, CodeError> {
    let upper = ch.to_ascii_uppercase();
    let mapped = match upper {
        'O' => '0',
        'I' | 'L' => '1',
        other => other,
    };
    ALPHABET
        .iter()
        .position(|&c| c as char == mapped)
        .map(|p| p as u8)
        .ok_or(CodeError::BadChar(ch))
}

/// 16 bit 标签 → 4 个字符：高位补 4 个 0，凑成 20 bit。
pub fn label_to_chars(label: u16) -> String {
    let v = u32::from(label);
    let groups = [(v >> 15) & 0x1f, (v >> 10) & 0x1f, (v >> 5) & 0x1f, v & 0x1f];
    groups.iter().map(|&g| ALPHABET[g as usize] as char).collect()
}

/// 4 个字符 → 16 bit 标签。超出 16 bit 就是错码，不是"尽力截断"。
pub fn label_from_chars(chars: &str) -> Result<u16, CodeError> {
    let mut v: u32 = 0;
    let mut count = 0;
    for ch in chars.chars() {
        if ch == '-' || ch == '_' || ch.is_whitespace() {
            continue;
        }
        v = (v << 5) | u32::from(decode_char(ch)?);
        count += 1;
    }
    if count != LABEL_CHARS {
        return Err(CodeError::BadLength);
    }
    u16::try_from(v).map_err(|_| CodeError::BadLength)
}

/// 门牌号：6 个字符，大小写与 `O/I/L` 别名都归一化。
pub fn is_room_code(raw: &str) -> bool {
    let mut count = 0;
    for ch in raw.chars() {
        if ch == '-' || ch == '_' || ch.is_whitespace() {
            continue;
        }
        if decode_char(ch).is_err() {
            return false;
        }
        count += 1;
    }
    count == ROOM_LEN
}

/// 把门牌号归一化到规范形式（大写、无分隔符、别名已替换）。
pub fn normalize_room(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_whitespace() && *c != '-' && *c != '_')
        .map(|c| {
            let upper = c.to_ascii_uppercase();
            match upper {
                'O' => '0',
                'I' | 'L' => '1',
                other => other,
            }
        })
        .collect()
}

fn sanitize(input: &str) -> String {
    input.chars().filter(|c| !c.is_whitespace()).collect()
}

/// 顺序读字节，越界就报错，不做任何"猜"。
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CodeError> {
        if self.pos + n > self.buf.len() {
            return Err(CodeError::BadLength);
        }
        let slice = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }
}

#[cfg(test)]
mod door_label_tests {
    use super::*;

    /// 门牌号必须自带节点：这一串发过去，对方不查节点列表也能敲。
    #[test]
    fn the_door_label_carries_the_node() {
        let code = "36YS5JNX1YAS0K5XJAJ6MBSEJ12WW";
        let node = NodeCode::decode(code).expect("节点码");
        let label = door_label(code, "h354jk");
        assert_eq!(label, Invite::full(&node, "H354JK").unwrap());
        assert!(label.contains('.'));
        match Invite::parse(&label).unwrap() {
            Invite::Full { room, .. } => assert_eq!(room, "H354JK"),
            other => panic!("应当解析成完整邀请，实际 {other:?}"),
        }
    }

    /// 没有节点码时退回房间码：宁可短，也别给一串解析不出来的。
    #[test]
    fn without_a_node_code_the_label_stays_short() {
        assert_eq!(door_label("", "h354jk"), "H354JK");
        assert_eq!(door_label("不是码", "H354JK"), "H354JK");
    }
}
