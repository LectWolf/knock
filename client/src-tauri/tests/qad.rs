//! 用真中继试一次「QUIC 地址发现」（打洞辅助的那一半）。
//!
//! 判定依据是 iroh 的网络报告 `udp_v4` / `udp_v6`：只要有一轮 QUIC 往返成功就是 true。
//!
//! ```console
//! KNOCK_RELAY_URL=https://node-tw.mcii.cc:30991 cargo test --test qad -- --ignored --nocapture
//! ```
//!
//! 没设 `KNOCK_RELAY_URL` 就跳过（不算失败）。

use std::time::{Duration, Instant};

use iroh::Watcher as _;
use knock::pipe;

#[tokio::test]
#[ignore]
async fn the_relay_answers_quic_address_discovery() {
    let Ok(raw) = std::env::var("KNOCK_RELAY_URL") else {
        eprintln!("没设 KNOCK_RELAY_URL，跳过");
        return;
    };

    let mode = pipe::relay_mode(Some(&raw)).expect("中继地址应当可用");
    let ep = pipe::bind_endpoint_with(mode).await.expect("端点应当起来");

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::from("还没拿到网络报告");
    let mut ok = false;
    while Instant::now() < deadline {
        if let Some(report) = ep.net_report().get() {
            last = format!(
                "udp_v4={} udp_v6={} global_v4={:?} global_v6={:?}",
                report.udp_v4, report.udp_v6, report.global_v4, report.global_v6
            );
            if report.udp_v4 || report.udp_v6 {
                ok = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    eprintln!("网络报告：{last}");
    eprintln!("中继地址：{raw}");
    assert!(ok, "30 秒内没有一次 QAD 成功——中继的 QUIC（UDP）多半没通：{last}");
    if let Some(report) = ep.net_report().get() {
        if let Some(a) = report.global_v4 {
            eprintln!("公网 IPv4（中继看到的）：{a}");
        }
        if let Some(a) = report.global_v6 {
            eprintln!("公网 IPv6（中继看到的）：{a}");
        }
    }

    ep.close().await;
}
