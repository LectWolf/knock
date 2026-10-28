//! 跨语言集成测试：**Rust 客户端 ↔ Go 节点**。
//!
//! 默认跳过（`#[ignore]`），因为它要一个真的在跑的节点。用 `sh scripts/cross.sh` 跑。
//!
//! 这个测试验的是一件单元测试验不了的事：Go 那边打印出来的节点码，
//! Rust 这边解出来的 pin，能不能验过 Go 接口自报的公钥。
//! 两边的实现只要有一点漂，这里就会红。

use knock::invite::Invite;
use knock::room::{pin_matches, HttpRoomClient, RoomClient, RoomError};

fn base_url() -> String {
    std::env::var("KNOCK_NODE_URL").expect("需要 KNOCK_NODE_URL（scripts/cross.sh 会提供）")
}

fn node_code() -> String {
    std::env::var("KNOCK_NODE_CODE").expect("需要 KNOCK_NODE_CODE（scripts/cross.sh 会提供）")
}

const HOST_ID: &str = "abababababababababababababababababababababababababababababababab";

#[test]
#[ignore = "需要一个真的 Go 节点；用 scripts/cross.sh 跑"]
fn rust_client_talks_to_the_go_node() {
    let client = HttpRoomClient::new(base_url());

    // —— 1. 节点码里的 pin，必须能验过接口自报的公钥 ——
    let parsed = Invite::parse(&node_code()).expect("解析 Go 打印的节点码");
    let node = parsed.node().expect("节点码里应当有节点").clone();
    let node_pin = node.pin;

    // —— 2. 开门 ——
    let room = client.open(25565).expect("开门");
    assert_eq!(room.code.len(), 6, "门牌号必须是 6 位");
    assert_eq!(room.code.to_uppercase(), room.code, "门牌号应当是大写规范形式");

    let token = room.token.clone().expect("开门响应必须带令牌");
    assert_eq!(token.len(), 64, "令牌是 32 字节 hex");

    let node_id = room.node_id.clone().expect("响应里必须有 node_id");
    assert!(
        pin_matches(&node_pin, &node_id),
        "节点自报的公钥和节点码里的 pin 对不上 —— 身份链断了"
    );

    // —— 3. 查码不该带令牌 ——
    let looked = client.lookup(&room.code).expect("查码");
    assert!(looked.token.is_none(), "查码响应绝不能带令牌");
    assert!(looked.host_id.is_none(), "还没 announce，不该有房主身份");

    // —— 4. announce 房主身份与候选地址 ——
    let addrs = vec!["203.0.113.7:5000".to_string(), "203.0.113.7:5001".to_string()];
    client
        .announce(&room.code, &token, HOST_ID, &addrs)
        .expect("announce");

    let after = client.lookup(&room.code).expect("announce 后查码");
    assert_eq!(after.host_id.as_deref(), Some(HOST_ID));
    assert_eq!(after.host_addrs, addrs, "候选地址应当原样取回");

    // —— 5. 错令牌必须被拒 ——
    match client.announce(&room.code, "deadbeef", HOST_ID, &["203.0.113.7:9999".to_string()]) {
        Err(RoomError::BadToken) => {}
        other => panic!("错令牌应当 BadToken，实际 {other:?}"),
    }

    // —— 6. 关门之后查不到 ——
    client.close(&room.code).expect("关门");
    match client.lookup(&room.code) {
        Err(RoomError::NotFound) => {}
        other => panic!("关门后应当 NotFound，实际 {other:?}"),
    }
}

#[test]
#[ignore = "需要一个真的 Go 节点；用 scripts/cross.sh 跑"]
fn unknown_room_is_not_found() {
    let client = HttpRoomClient::new(base_url());
    match client.lookup("AAAAAA") {
        Err(RoomError::NotFound) => {}
        other => panic!("不存在的门牌号应当 NotFound，实际 {other:?}"),
    }
}
