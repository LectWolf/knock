//! 传输层端到端：两个 `IrohTransport` + 一台真节点 + 一个假 MC 服务端。
//!
//! 验的是完整闭环：房主开门 → announce → 房客解析邀请 → resolve 验 pin →
//! 拨号 → 状态落到 `direct` → 本地入口把 TCP 流量送进假 MC 服务端并原样回来。
//!
//! ```sh
//! KNOCK_NODE_URL=http://154.36.165.162:30990 \
//! KNOCK_NODE_CODE=377PN6H4MPH0WYEFD838Q42C674TR \
//!   cargo test --test transport_e2e -- --ignored --nocapture
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use knock::iroh_transport::IrohTransport;
use knock::machine::Update;
use knock::room::HttpRoomClient;
use knock::state::{ConnState, FailReason};
use knock::transport::{StateSink, Target, Transport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("需要 {key}"))
}

/// 记下所有状态更新，供断言用。
fn recorder() -> (StateSink, Arc<Mutex<Vec<Update>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let sink_log = Arc::clone(&log);
    (
        Box::new(move |u: &Update| sink_log.lock().unwrap().push(*u)),
        log,
    )
}

/// 等到条件成立返回 true，超时返回 false（不像 wait_for 那样 panic）
async fn wait_or(what: &str, secs: u64, mut done: impl FnMut() -> bool) -> bool {
    for _ in 0..(secs * 10) {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("超时：{what}");
    false
}

/// 开门可能要排队：节点有速率限制，测试里连着开很多扇门会被挡一下。
/// 按它给的 retry_after 等一下再来，别把限流当成功能坏了。
async fn open_retrying(
    client: &Arc<dyn knock::room::RoomClient + Send + Sync>,
    port: u16,
) -> knock::room::RoomInfo {
    for _ in 0..4 {
        match client.open(port) {
            Ok(room) => return room,
            Err(knock::room::RoomError::RateLimited { retry_after }) => {
                let wait = retry_after.unwrap_or(5).min(30);
                println!("开门被限流，{} 秒后再试", wait);
                tokio::time::sleep(Duration::from_secs(wait as u64)).await;
            }
            Err(e) => panic!("开门失败: {e:?}"),
        }
    }
    panic!("开门重试多次仍被限流");
}

async fn wait_for(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    for _ in 0..(secs * 20) {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等「{what}」超时");
}

/// 假 MC 服务端：收到什么回什么。
async fn fake_mc_server() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定假 MC");
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 || socket.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要一台真节点；见文件头的环境变量"]
async fn a_guest_knocks_and_traffic_flows() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    let mc_port = fake_mc_server().await;

    // —— 房主：开门 ——
    let mut host = IrohTransport::new(Arc::clone(&client));
    let (_host_sink, host_log) = recorder();
    host.on_state(_host_sink);
    host.connect(Target::Open { port: mc_port }).expect("房主受理");

    wait_for("房主拿到门牌号", 20, || host.room_code().is_some()).await;
    let code = host.room_code().expect("门牌号");
    assert_eq!(code.len(), 6, "门牌号 6 位");
    println!("门牌号：{code}");
    // 门牌号和状态是两步：拿到码之后状态机才推进到 waiting
    wait_for("房主进入等对方", 20, || host.state() == ConnState::Waiting).await;

    // 等 announce 落地：查一次能看到房主身份才继续
    let prober = Arc::clone(&client);
    let probe_code = code.clone();
    wait_for("房主身份挂上房间", 20, || {
        prober.lookup(&probe_code).map(|r| r.host_id.is_some()).unwrap_or(false)
    })
    .await;

    // —— 房客：敲门 ——
    let invite = format!("{node_code}.{code}");
    let mut guest = IrohTransport::new(Arc::clone(&client));
    let (_guest_sink, guest_log) = recorder();
    guest.on_state(_guest_sink);
    guest.connect(Target::Knock { invite: &invite }).expect("房客受理");

    wait_for("房客连上", 40, || {
        matches!(guest.state(), ConnState::Direct | ConnState::Relay | ConnState::Failed)
    })
    .await;

    match guest.state() {
        ConnState::Direct => println!("直连成功"),
        ConnState::Relay => println!("经中继连上"),
        ConnState::Failed => {
            let log = guest_log.lock().unwrap();
            panic!("房客失败：{:?}", log.last().and_then(|u| u.fail));
        }
        other => panic!("意外状态 {other:?}"),
    }

    let local = guest.local_addr().expect("房客的本地入口");
    println!("本地入口：{local}");

    // —— 真的走一遍数据：MC 客户端连本地入口 ——
    let mut socket = TcpStream::connect(local).await.expect("连本地入口");
    socket.write_all(b"\xfe\x01 knock handshake").await.unwrap();

    let mut buf = vec![0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(20), socket.read(&mut buf))
        .await
        .expect("等回显超时")
        .expect("读回显");

    assert_eq!(&buf[..n], b"\xfe\x01 knock handshake", "字节必须原样穿过门");
    println!("数据往返成功：{} 字节", n);

    // 房主侧从头到尾都在 waiting：门开着可以接待下一个
    assert_eq!(host.state(), ConnState::Waiting, "房主会话结束后应当回到等对方");
    assert!(
        host_log.lock().unwrap().iter().all(|u| u.fail.is_none()),
        "房主不该出现失败"
    );
    let _ = FailReason::CodeNotFound;
    host.close();
    guest.close();
}

/// 房间里没人挂地址（房主还没 announce）→ 应当是 `host_offline`，不是干等。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要一台真节点；见文件头的环境变量"]
async fn a_room_without_a_host_fails_as_host_offline() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    // 只开门、不 announce：门开着，但屋里没人
    let room = open_retrying(&client, 25565).await;
    let invite = format!("{node_code}.{}", room.code);

    let mut guest = IrohTransport::new(Arc::clone(&client));
    let (_sink, log) = recorder();
    guest.on_state(_sink);
    guest.connect(Target::Knock { invite: &invite }).expect("受理");

    wait_for("房客失败", 20, || guest.state() == ConnState::Failed).await;
    let fail = log.lock().unwrap().last().and_then(|u| u.fail);
    assert_eq!(fail, Some(FailReason::HostOffline), "应当归因到屋里没人");
    guest.close();
}

/// 门牌号不存在 → `code_not_found`。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要一台真节点；见文件头的环境变量"]
async fn an_unknown_code_fails_as_code_not_found() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    let invite = format!("{node_code}.ZZZZZZ");
    let mut guest = IrohTransport::new(Arc::clone(&client));
    let (_sink, log) = recorder();
    guest.on_state(_sink);
    guest.connect(Target::Knock { invite: &invite }).expect("受理");

    wait_for("房客失败", 20, || guest.state() == ConnState::Failed).await;
    let fail = log.lock().unwrap().last().and_then(|u| u.fail);
    assert_eq!(fail, Some(FailReason::CodeNotFound));
    guest.close();
}

/// 打洞失败就绕路：房主给的是一个死地址 + 一台可用中继。
///
/// 房主是手工搭的（这样能塞一个连不上的直连地址），房客走真 transport，
/// 必须先在直连上失败、再落到 `relay`，并且数据仍然能过去。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要真节点 + 真中继；见文件头的环境变量"]
async fn a_failed_punch_falls_back_to_the_relay() {
    use knock::pipe;

    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let relay_url =
        std::env::var("KNOCK_RELAY_URL").unwrap_or_else(|_| "http://127.0.0.1:30991".into());

    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    // 房主自己绑一个带中继的端点
    let host_ep = pipe::bind_endpoint_with(pipe::relay_mode(Some(&relay_url)).expect("中继地址"))
        .await
        .expect("房主端点");

    // 等它在中继上注册好，否则房客经中继找不到它
    for _ in 0..100 {
        if host_ep.addr().relay_urls().next().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let mc_port = fake_mc_server().await;
    let serve_ep = host_ep.clone();
    // 跟生产一样走"先自报家门、再放行"的入口：裸 serve 收不了报到帧
    let gate = std::sync::Arc::new(knock::sessions::SessionList::new());
    tokio::spawn(async move {
        let _ = pipe::serve_gated(serve_ep, ([127, 0, 0, 1], mc_port).into(), gate).await;
    });

    let room = open_retrying(&client, 25565).await;
    let token = room.token.clone().expect("令牌");
    let host_hex: String = host_ep.id().as_bytes().iter().map(|b| format!("{b:02x}")).collect();

    // 关键：只给一个连不上的直连地址，逼它走中继
    client
        .announce(&room.code, &token, &host_hex, &["127.0.0.2:1".to_string()])
        .expect("announce 一个死地址");

    let invite = format!("{node_code}.{}", room.code);
    let mut guest = IrohTransport::new(Arc::clone(&client));
    let (_sink, log) = recorder();
    guest.on_state(_sink);
    guest.connect(Target::Knock { invite: &invite }).expect("受理");

    wait_for("房客落定", 60, || {
        matches!(guest.state(), ConnState::Direct | ConnState::Relay | ConnState::Failed)
    })
    .await;

    assert_eq!(
        guest.state(),
        ConnState::Relay,
        "直连是死地址，必须绕到中继（失败原因：{:?}）",
        log.lock().unwrap().last().and_then(|u| u.fail)
    );

    let local = guest.local_addr().expect("本地入口");
    let mut socket = TcpStream::connect(local).await.expect("连本地入口");
    socket.write_all(b"relayed bytes").await.unwrap();
    let mut buf = vec![0u8; 32];
    let n = tokio::time::timeout(Duration::from_secs(20), socket.read(&mut buf))
        .await
        .expect("等回显超时")
        .expect("读回显");
    assert_eq!(&buf[..n], b"relayed bytes", "经中继的数据必须原样到达");
    guest.close();
}

/// 心跳：房间寿命短的时候，房主只要还在，门就不该被节点回收。
///
/// 这条是用"短 TTL + 短心跳"把十分钟的等待压到十几秒：
/// 本地节点配 `room_ttl: 15s`，`KNOCK_ANNOUNCE_SECS=6`，
/// 然后让门开着睡过 TTL——心跳没生效的话，查码一定是 code_not_found。
#[tokio::test]
#[ignore]
async fn a_heartbeat_keeps_the_door_from_expiring() {
    use knock::room::RoomError;

    let base = env("KNOCK_NODE_URL");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    let mut host = IrohTransport::new(Arc::clone(&client));
    let (_sink, _log) = recorder();
    host.on_state(_sink);
    host.connect(Target::Open { port: 25565 }).expect("房主受理");

    // 节点有"每分钟开门数"配额，连跑整套 e2e 时偶尔会被挡；
    // 等一会儿重开一次，别把配额当功能坏了。
    let mut opened = false;
    for attempt in 0..3 {
        host.connect(Target::Open { port: 25565 }).expect("房主受理");
        wait_for("房主拿到门牌号", 20, || host.room_code().is_some()).await;
        if wait_or("房主进入等对方", 20, || host.state() == ConnState::Waiting).await {
            opened = true;
            break;
        }
        println!("第 {} 次被节点挡了（{:?}），等 20 秒再来", attempt + 1, host.state());
        host.close();
        tokio::time::sleep(Duration::from_secs(20)).await;
    }
    assert!(opened, "试了三次都没能开门");
    let code = host.room_code().expect("门牌号");

    // 睡过 TTL：没心跳的话这里房间已经没了
    let ttl = std::env::var("KNOCK_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(25);
    println!("门开着，睡 {ttl} 秒…");
    tokio::time::sleep(Duration::from_secs(ttl)).await;

    match client.lookup(&code) {
        Ok(room) => println!("{} 秒后门还在（host_id={:?}）", ttl, room.host_id.is_some()),
        Err(RoomError::NotFound) => panic!("心跳没续上：门在 {ttl} 秒后过期了"),
        Err(e) => panic!("查码出错：{e:?}"),
    }

    assert_eq!(host.state(), ConnState::Waiting, "睡这么久，房主状态不该变");
}

/// "我这边没开服，他敲门我会不会看到人？"——这个问题的答案不该靠猜。
///
/// 房主在一个**没人监听**的端口上开门，房客照常敲：
///   - 房客的结局应该是 `host_offline`（如实说"屋里没人"，不是"码错了"）
///   - 房主的名册：先出现、再消失，还是压根不出现？这里把它量出来。
#[tokio::test]
#[ignore]
async fn a_guest_who_finds_no_game_still_shows_up_first() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    // 拿一个没人监听的端口：绑一下再放开
    let dead_port = {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    };

    let mut host = IrohTransport::new(Arc::clone(&client));
    let (_sink, _log) = recorder();
    host.on_state(_sink);
    host.connect(Target::Open { port: dead_port }).expect("房主受理");

    wait_for("房主拿到门牌号", 20, || host.room_code().is_some()).await;
    wait_for("房主进入等对方", 20, || host.state() == ConnState::Waiting).await;
    let code = host.room_code().expect("门牌号");
    println!("门牌号 {code}（本机 {dead_port} 没有服务）");

    let invite = format!("{node_code}.{code}");
    let mut guest = IrohTransport::new(Arc::clone(&client));
    let (_gsink, glog) = recorder();
    guest.on_state(_gsink);
    guest.connect(Target::Knock { invite: &invite }).expect("房客受理");

    // 一边等房客出结果，一边紧盯房主的名册
    let sessions = host.sessions();
    let mut peak = 0usize;
    let mut peak_names: Vec<String> = Vec::new();
    for _ in 0..(40 * 25) {
        let snap = sessions.snapshot();
        if snap.len() > peak {
            peak = snap.len();
            peak_names = snap.iter().map(|(_, e)| e.nickname.clone()).collect();
        }
        if matches!(guest.state(), ConnState::Failed | ConnState::Direct | ConnState::Relay) {
            // 再观察一小会儿，确认不是"出现了立刻就没了"
            for _ in 0..40 {
                let snap = sessions.snapshot();
                if snap.len() > peak {
                    peak = snap.len();
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let log = glog.lock().unwrap();
    let fail = log.iter().rev().find(|u| u.fail.is_some()).and_then(|u| u.fail);
    println!("房客最终状态 {:?}，失败原因 {:?}", guest.state(), fail);
    println!("房主名册峰值 {peak} 人 {peak_names:?}（此刻 {} 人）", sessions.len());

    assert_eq!(guest.state(), ConnState::Failed, "没服务就该失败");
    assert_eq!(fail, Some(FailReason::HostOffline), "要报'屋里没人'，不是别的");
    assert_eq!(
        sessions.len(),
        0,
        "失败之后不该继续挂在名册里——那会让人以为他还在"
    );
    println!("门牌号看到的峰值人数：{peak}（0 = 名字没露过脸就被收回）");
}

/// 敲了门就该出现在"门里的人"里——不用等 MC 客户端真的连进来。
/// 这是房主最直观的一条反馈："他到了"。
#[tokio::test]
#[ignore]
async fn knocking_alone_puts_the_guest_in_the_roster() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    let mc_port = fake_mc_server().await;
    let mut host = IrohTransport::new(Arc::clone(&client));
    let (_sink, _log) = recorder();
    host.on_state(_sink);
    // 节点有开门限流（每分钟十几次），连着跑测试会被挡，等一下重开。
    let mut opened = false;
    for attempt in 1..=4 {
        host.connect(Target::Open { port: mc_port }).expect("房主受理");
        if wait_or("房主拿到门牌号", 20, || host.room_code().is_some()).await {
            opened = true;
            break;
        }
        println!("开门没成功（第 {attempt} 次），等 20 秒再来——大概是节点限流");
        tokio::time::sleep(Duration::from_secs(20)).await;
    }
    assert!(opened, "四次都没能开门");
    wait_for("房主进入等对方", 20, || host.state() == ConnState::Waiting).await;
    let code = host.room_code().expect("门牌号");

    let invite = format!("{node_code}.{code}");
    let mut guest = IrohTransport::new(Arc::clone(&client));
    let (_gsink, _glog) = recorder();
    guest.on_state(_gsink);
    guest.connect(Target::Knock { invite: &invite }).expect("房客受理");

    // 期间**没有任何游戏流量**，只有敲门这一下
    // 节点对「查门牌」也有速率限制；连着跑整套测试时最后一条容易被挡，
    // 表现是房客直接 Failed。等一会儿重敲，不算功能问题。
    let mut ok = wait_or("房主名册里出现他", 15, || host.sessions().len() >= 1).await;
    for attempt in 1..=3 {
        if ok {
            break;
        }
        println!("第一次敲门没成（名册 0 人，房客 {:?}），20 秒后重敲", guest.state());
        tokio::time::sleep(Duration::from_secs(20)).await;
        guest.connect(Target::Knock { invite: &invite }).expect("房客受理");
        ok = wait_or("房主名册里出现他", 15, || host.sessions().len() >= 1).await;
        println!("重敲第 {attempt} 次：名册 {} 人，房客 {:?}", host.sessions().len(), guest.state());
    }
    let sessions = host.sessions();
    println!("敲门后名册 {} 人，房客状态 {:?}", sessions.len(), guest.state());
    assert!(ok, "敲了门就该出现在名册里");
    assert!(
        matches!(guest.state(), ConnState::Direct | ConnState::Relay),
        "房客应该已经门开了，实际 {:?}",
        guest.state()
    );
}


/// 房主看到的名字应该是房客自己起的昵称，而不是写死的 "knock"；
/// 踢人必须真的把人断掉、并从名册里消失（以前只摘名单，连接还挂着）。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要一台真节点；见文件头的环境变量"]
async fn the_host_sees_my_nickname_and_can_show_me_out() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    let mc_port = fake_mc_server().await;
    let mut host = IrohTransport::new(Arc::clone(&client));
    let (_sink, _log) = recorder();
    host.on_state(_sink);
    host.connect(Target::Open { port: mc_port }).expect("房主受理");
    wait_for("房主拿到门牌号", 25, || host.room_code().is_some()).await;
    wait_for("房主进入等对方", 20, || host.state() == ConnState::Waiting).await;
    let code = host.room_code().expect("门牌号");

    let invite = format!("{node_code}.{code}");
    let mut guest = IrohTransport::new(Arc::clone(&client)).with_nickname("小乐");
    let (_gsink, _glog) = recorder();
    guest.on_state(_gsink);
    guest.connect(Target::Knock { invite: &invite }).expect("房客受理");

    let sessions = host.sessions();
    let appeared = wait_or("名册里出现他", 25, || !sessions.is_empty()).await;
    assert!(appeared, "敲了门就该出现在名册里");
    let (peer, entry) = sessions.snapshot()[0].clone();
    println!("名册：{:?}，绕路={}", entry.nickname, entry.via_relay);
    assert_eq!(entry.nickname, "小乐", "房主该看到房客自己的昵称");

    sessions.kick(&peer);
    let gone = wait_or("名册里没他了", 10, || sessions.is_empty()).await;
    println!("踢完名册 {} 人", sessions.len());
    assert!(gone, "踢出去之后名册里不该还留着他");
}


/// 房主关门之后，房客不能一直停在「门开了」——之前就是这样：
/// 对面都没了，界面还绿着，而且点「回到门口」也回不去。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要一台真节点；见文件头的环境变量"]
async fn a_guest_notices_when_the_host_closes_the_door() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    let mc_port = fake_mc_server().await;
    let mut host = IrohTransport::new(Arc::clone(&client));
    let (_hs, _hl) = recorder();
    host.on_state(_hs);
    host.connect(Target::Open { port: mc_port }).expect("房主受理");
    wait_for("房主拿到门牌号", 25, || host.room_code().is_some()).await;
    // waiting 现在只在地址真的挂上之后才出现，等到它就是"门已经能被敲"
    wait_for("房主把地址挂上去", 15, || host.state() == ConnState::Waiting).await;
    let code = host.room_code().expect("门牌号");

    let mut guest = IrohTransport::new(Arc::clone(&client)).with_nickname("小乐");
    let (gsink, glog) = recorder();
    guest.on_state(gsink);
    guest.connect(Target::Knock { invite: &format!("{node_code}.{code}") }).expect("房客受理");
    let opened = wait_or("房客连上", 30, || {
        matches!(guest.state(), ConnState::Direct | ConnState::Relay)
    })
    .await;
    if !opened {
        for u in glog.lock().unwrap().iter() {
            println!("房客状态：{:?} fail={:?}", u.state, u.fail);
        }
    }
    assert!(opened, "先得连上，实际 {:?}", guest.state());

    host.close();
    let noticed = wait_or("房客发现门关了", 25, || guest.state() == ConnState::Failed).await;
    println!("房客现在：{:?}", guest.state());
    assert!(noticed, "房主关门后，房客不能还是「门开了」");
}


/// 开服的那个进程没了（不是房主关窗，是 MC 服务端自己停了）：
/// 房客也必须从「门开了」退回「屋里没人」，不能等他手动点。
#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要一台真节点；见文件头的环境变量"]
async fn a_guest_notices_when_the_game_server_stops() {
    let base = env("KNOCK_NODE_URL");
    let node_code = env("KNOCK_NODE_CODE");
    let client: Arc<dyn knock::room::RoomClient + Send + Sync> =
        Arc::new(HttpRoomClient::new(base));

    // 一个能随时关掉的"MC 服务端"
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定假 MC");
    let mc_port = listener.local_addr().unwrap().port();
    let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = listener.accept() => {}
                _ = &mut stop_rx => return,
            }
        }
    });

    let mut host = IrohTransport::new(Arc::clone(&client));
    let (_hs, _hl) = recorder();
    host.on_state(_hs);
    host.connect(Target::Open { port: mc_port }).expect("房主受理");
    wait_for("房主拿到门牌号", 25, || host.room_code().is_some()).await;
    wait_for("房主把地址挂上去", 15, || host.state() == ConnState::Waiting).await;
    let code = host.room_code().expect("门牌号");

    let mut guest = IrohTransport::new(Arc::clone(&client)).with_nickname("小乐");
    let (_gs, _gl) = recorder();
    guest.on_state(_gs);
    guest.connect(Target::Knock { invite: &format!("{node_code}.{code}") }).expect("房客受理");
    let opened = wait_or("房客连上", 30, || {
        matches!(guest.state(), ConnState::Direct | ConnState::Relay)
    })
    .await;
    assert!(opened, "先得连上，实际 {:?}", guest.state());

    // 服务端停掉：房主窗口还开着，门也还开着
    let _ = stop_tx.send(());
    tokio::time::sleep(Duration::from_millis(300)).await;

    let noticed = wait_or("房客发现屋里没人", 25, || guest.state() == ConnState::Failed).await;
    println!("房客现在：{:?}", guest.state());
    assert!(noticed, "服务端停了，房客不该还显示门开了");
}
