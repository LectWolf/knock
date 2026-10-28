//! 生成 `spec/invite-v1.tsv`。
//!
//! 用法：
//!     cargo run --example gen_vectors > ../../spec/invite-v1.tsv
//!
//! 这份 TSV 是 Rust 与 Go 的共同基准。改了编码就重新生成，然后两边的测试都会替你检查。

use knock::invite::NodeCode;

struct Case {
    name: &'static str,
    label: u16,
    ipv4: Option<[u8; 4]>,
    ipv6: Option<[u8; 16]>,
    domain: Option<&'static str>,
    port: u16,
    pin: &'static str,
    relay: bool,
}

fn hex_bytes(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "hex 长度必须是偶数: {s}");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("合法 hex"))
        .collect()
}

fn pin8(s: &str) -> [u8; 8] {
    let v = hex_bytes(s);
    assert_eq!(v.len(), 8, "pin 必须是 8 字节");
    let mut out = [0u8; 8];
    out.copy_from_slice(&v);
    out
}

fn main() {
    let cases = vec![
        Case {
            name: "v4-relay",
            label: 0x1234,
            ipv4: Some([203, 0, 113, 7]),
            ipv6: None,
            domain: None,
            port: 443,
            pin: "0011223344556677",
            relay: true,
        },
        Case {
            name: "v4-signal-only",
            label: 1,
            ipv4: Some([198, 51, 100, 9]),
            ipv6: None,
            domain: None,
            port: 8080,
            pin: "DEADBEEFCAFEBABE",
            relay: false,
        },
        Case {
            name: "v6-only",
            label: 0x00ff,
            ipv4: None,
            ipv6: Some([
                0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01,
            ]),
            domain: None,
            port: 443,
            pin: "FFEEDDCCBBAA9988",
            relay: true,
        },
        Case {
            name: "domain-max-label",
            label: 0xffff,
            ipv4: None,
            ipv6: None,
            domain: Some("relay.example.com"),
            port: 443,
            pin: "8899AABBCCDDEEFF",
            relay: true,
        },
        Case {
            name: "dual-all",
            label: 0,
            ipv4: Some([203, 0, 113, 7]),
            ipv6: Some([0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01]),
            domain: Some("node.example.org"),
            port: 8443,
            pin: "0123456789ABCDEF",
            relay: true,
        },
    ];

    println!("# spec/invite-v1.tsv —— 由 `cargo run --example gen_vectors` 生成，Rust 与 Go 共用这份基准。");
    println!("# 空字段表示该地址不存在。bytes_hex 是不含任何编码的原始字节，可直接人工核对布局。");
    println!("# name\tlabel\tipv4\tipv6\tdomain\tport\tpin_hex\trelay\tbytes_hex\tcode\troom\tfull_invite\tshort_invite");

    const ROOM: &str = "A7K2M9";

    for case in cases {
        let node = NodeCode {
            label: case.label,
            ipv4: case.ipv4,
            ipv6: case.ipv6,
            domain: case.domain.map(|s| s.to_string()),
            port: case.port,
            pin: pin8(case.pin),
            relay_allowed: case.relay,
        };

        let bytes = node.to_bytes().expect("编码必须成功");
        let bytes_hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
        let code = node.encode().expect("base32 必须成功");
        let full = knock::invite::Invite::full(&node, ROOM).expect("完整邀请");
        let short = knock::invite::Invite::short(&node, ROOM);

        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            case.name,
            case.label,
            case.ipv4
                .map(|v| format!("{}.{}.{}.{}", v[0], v[1], v[2], v[3]))
                .unwrap_or_default(),
            case.ipv6.map(format_ipv6).unwrap_or_default(),
            case.domain.unwrap_or_default(),
            case.port,
            case.pin,
            if case.relay { "true" } else { "false" },
            bytes_hex,
            code,
            ROOM,
            full,
            short,
        );
    }
}

/// 只为了在 TSV 里可读；测试里会逐字节比对，不依赖这个字符串的格式。
fn format_ipv6(b: [u8; 16]) -> String {
    let mut groups = [0u16; 8];
    for i in 0..8 {
        groups[i] = (u16::from(b[i * 2]) << 8) | u16::from(b[i * 2 + 1]);
    }
    groups
        .iter()
        .map(|g| format!("{g:04x}"))
        .collect::<Vec<_>>()
        .join(":")
}
