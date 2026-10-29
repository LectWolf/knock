//! 当"房主那台"用：真的开一扇门（游戏端口给 6702），把门牌号打出来，然后撑着。
//!
//! 用法：
//!   KNOCK_NODE_URL=http://… KNOCK_PORT=6702 KNOCK_HOLD_SECS=90 \
//!     cargo test --test live_host -- --ignored --nocapture [--test-threads=1]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use knock::iroh_transport::IrohTransport;
use knock::machine::Update;
use knock::room::HttpRoomClient;
use knock::state::ConnState;
use knock::transport::{Target, Transport};

fn env_or(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}

#[test]
#[ignore]
fn hold_a_live_door() {
    let node = env_or("KNOCK_NODE_URL", "http://154.36.165.162:30990");
    let port: u16 = env_or("KNOCK_PORT", "6702").parse().expect("端口");
    let hold: u64 = env_or("KNOCK_HOLD_SECS", "90").parse().expect("秒数");

    let client: Arc<dyn knock::room::RoomClient + Send + Sync> = Arc::new(HttpRoomClient::new(node));
    let mut t = IrohTransport::new(client);
    let log: Arc<Mutex<Vec<Update>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&log);
    t.on_state(Box::new(move |u: &Update| sink.lock().unwrap().push(*u)));

    t.connect(Target::Open { port }).expect("开门请求");

    let deadline = std::time::Instant::now() + Duration::from_secs(hold);
    let mut code: Option<String> = None;
    let mut announced = false;
    while std::time::Instant::now() < deadline {
        if code.is_none() {
            code = t.room_code();
        }
        if let Some(c) = &code {
            if !announced {
                println!("门牌号 {c}  （游戏端口 {port}）");
                announced = true;
            }
        }
        let st = t.state();
        let n = t.sessions().len();
        if announced && std::time::Instant::now().elapsed().as_secs() % 5 == 0 {
            println!("状态 {st:?}，门里 {n} 人");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
    println!("房主收工：门里最后 {} 人", t.sessions().len());
    if let Some(u) = log.lock().unwrap().last() {
        println!("最后状态 {:?} {:?}", u.state, u.fail);
    }
    t.close();
}
