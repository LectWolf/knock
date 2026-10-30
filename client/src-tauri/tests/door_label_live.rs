//! 门牌号必须是完整邀请：`<节点码>.<房间码>`。
//!
//! 这个测试专门盯一个真实踩过的坑——老配置里没存节点码时，
//! 界面只显示了 6 位房间码，朋友拿到手没法直接敲。
//!
//! ```console
//! KNOCK_NODE_URL=http://node-tw.mcii.cc:30990 cargo test --test door_label_live -- --ignored --nocapture
//! ```

use knock::{invite, nodes};

#[test]
#[ignore = "需要一台真节点；见文件头"]
fn a_missing_node_code_is_filled_from_the_node_itself() {
    let url = std::env::var("KNOCK_NODE_URL").expect("要设 KNOCK_NODE_URL");
    let dir = std::env::temp_dir().join("knock-door-label-live");
    let _ = std::fs::create_dir_all(&dir);

    // 设置里只有地址、没有节点码，列表缓存里也没这台机——最坏的情况
    let code = nodes::code_for(&dir, &url, "").expect("节点应当自报编码");
    assert_eq!(code.len(), 29, "节点码长度不对：{code}");

    let label = invite::door_label(&code, "r1tgcz");
    println!("门牌号：{label}");
    assert_eq!(label, format!("{code}.R1TGCZ"));
    assert!(label.contains('.'), "必须有分隔点，不然朋友粘不了整串");
}

#[test]
fn without_any_node_we_still_show_something() {
    // 完全没有节点时只能给短码，这不是 bug，但要保证别崩、别拼出怪东西
    assert_eq!(invite::door_label("", "r1tgcz"), "R1TGCZ");
}
