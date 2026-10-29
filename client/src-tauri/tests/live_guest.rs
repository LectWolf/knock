//! 当"第二台电脑"用：敲一扇**真·桌面客户端**开着的门，然后撑住连接。
//!
//! 用法（不需要界面）：
//!   KNOCK_NODE_URL=http://… KNOCK_INVITE=<节点码>.<门牌号> cargo test --test live_guest -- --ignored --nocapture
//!
//! 它的意义是：房主那边跑的是真实 exe（而不是测试里的假房主），
//! 这样"门里的人"、门开了之后的真实表现都能当场看到。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use knock::iroh_transport::IrohTransport;
use knock::state::FailReason;
use knock::transport::Target;
use knock::room::HttpRoomClient;
use knock::state::ConnState;
use knock::transport::Transport;
use knock::machine::Update;

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("缺环境变量 {k}"))
}

#[test]
#[ignore]
fn knock_a_live_door() {
    let node = env("KNOCK_NODE_URL");
    let invite = env("KNOCK_INVITE");
    let hold: u64 = std::env::var("KNOCK_HOLD_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(45);

    let client: Arc<dyn knock::room::RoomClient + Send + Sync> = Arc::new(HttpRoomClient::new(node));
    let mut t = IrohTransport::new(client);
    let seen: Arc<Mutex<Vec<Update>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    t.on_state(Box::new(move |u: &Update| sink.lock().unwrap().push(*u)));

    t.connect(Target::Knock { invite: &invite }).expect("受理");

    let deadline = std::time::Instant::now() + Duration::from_secs(hold);
    let mut reported = false;
    while std::time::Instant::now() < deadline {
        let st = t.state();
        if !reported && matches!(st, ConnState::Direct | ConnState::Relay | ConnState::Failed) {
            let fail = seen.lock().unwrap().last().and_then(|u| u.fail);
            println!("房客状态：{st:?} 失败原因：{fail:?}");
            reported = true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    println!("撑了 {hold} 秒，收工");
    t.close();
}
