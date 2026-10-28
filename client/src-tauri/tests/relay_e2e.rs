//! 中继路径的端到端测试：**只给 EndpointId，不给任何直连地址**。
//!
//! 这样两端唯一的通路就是中继——能传通，就说明中继真的在干活，
//! 而不是偷偷又打通了直连。
//!
//! 默认跳过，需要一台真在跑的中继：
//!
//! ```sh
//! KNOCK_RELAY_URL=http://127.0.0.1:30991 cargo test --test relay_e2e -- --ignored --nocapture
//! ```

use std::time::Duration;

use knock::pipe;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 一个只会回显的 TCP 服务，冒充 Minecraft 服务端。
async fn echo_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定回声服务");
    let addr = listener.local_addr().unwrap();
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
    addr
}

/// 端点是否已经在中继上注册。用 Debug 输出判断，避免依赖具体 API 形状。
fn has_relay(endpoint: &iroh::Endpoint) -> bool {
    format!("{:?}", endpoint.addr()).contains("Relay(")
}

async fn wait_for_relay(endpoint: &iroh::Endpoint, who: &str) {
    for _ in 0..300 {
        if has_relay(endpoint) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{who}端点 30 秒都没挂上中继");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "需要一台真在跑的中继；用 scripts/relay.sh 跑"]
async fn traffic_flows_through_the_relay() {
    let url = std::env::var("KNOCK_RELAY_URL").expect("需要 KNOCK_RELAY_URL");

    let host = pipe::bind_endpoint_with(pipe::relay_mode(Some(&url)).expect("中继地址合法"))
        .await
        .expect("房主端点");
    let guest = pipe::bind_endpoint_with(pipe::relay_mode(Some(&url)).expect("中继地址合法"))
        .await
        .expect("房客端点");

    // 等两端都真的挂上中继。中继是唯一通路，注册完成之前连不上是正常的——
    // 生产代码里同样必须等"在线"，不能靠 sleep 撞运气。
    wait_for_relay(&host, "房主").await;
    wait_for_relay(&guest, "房客").await;
    println!("房主地址: {:?}", host.addr());
    println!("房客地址: {:?}", guest.addr());

    let echo = echo_server().await;
    let host_ep = host.clone();
    tokio::spawn(async move {
        if let Err(e) = pipe::serve(host_ep, echo).await {
            println!("serve 退出: {e}");
        }
    });

    // 关键：EndpointAddr 里**只有身份**，没有任何 ip:port。
    // 有直连地址的话测的就不是中继了。
    // 身份 + 中继地址，**没有任何 ip:port**。这样唯一通路就是中继，
    // 也正是生产里"两边都出不去"时用的那条路。
    let relay_url: iroh::RelayUrl = url.parse().expect("中继地址");
    let node = iroh::EndpointAddr::new(host.id()).with_relay_url(relay_url);

    let local = pipe::listen_local(guest, node).await.expect("经中继建立通道");

    let mut socket = TcpStream::connect(local).await.expect("连本地入口");
    socket.write_all(b"hello through the relay").await.unwrap();

    let mut buf = vec![0u8; 32];
    let n = tokio::time::timeout(Duration::from_secs(20), socket.read(&mut buf))
        .await
        .expect("等回显超时")
        .expect("读回显");

    assert_eq!(&buf[..n], b"hello through the relay", "经中继的数据必须原样到达");
}
