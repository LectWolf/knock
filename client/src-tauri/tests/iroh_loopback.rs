//! 用**真的** iroh 端点验数据通路：两个端点在本机走 QUIC 直连，
//! 一头接假 MC 服务器（回声），另一头接"本机端口"，字节必须原样往返。
//!
//! 这个测试不需要公网、不需要中继、不需要节点：`RelayMode::Disabled` + 环回地址。
//! 它证明的是"MC 的 TCP 能被塞进 QUIC 送到对面"，也就是这产品最核心的那一下。

use std::net::{Ipv4Addr, SocketAddr};

use knock::pipe;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 假 MC 服务器：收到什么就回什么。
async fn echo_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("起回声服务");
    let addr = listener.local_addr().expect("回声地址");

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if socket.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });

    addr
}

/// 把端点自己的地址换成环回地址。
///
/// 端点绑在 0.0.0.0 上时，`addr()` 里可能是未指定地址，直接拿去连会失败；
/// 测试里我们只想知道"同一台机器上能不能连"，所以统一改成 127.0.0.1，端口照抄。
fn loopback_addr(endpoint: &iroh::Endpoint) -> iroh::EndpointAddr {
    let mut addr = iroh::EndpointAddr::new(endpoint.id());
    for socket in endpoint.addr().ip_addrs() {
        addr = addr.with_ip_addr(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), socket.port()));
    }
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bytes_travel_through_a_real_quic_connection() {
    let echo = echo_server().await;

    let host = pipe::bind_endpoint().await.expect("房主端点");
    let node_addr = loopback_addr(&host);
    tokio::spawn(pipe::serve(host, echo));

    let guest = pipe::bind_endpoint().await.expect("房客端点");
    let local = pipe::listen_local(guest, node_addr)
        .await
        .expect("房客侧本机端口");

    // 这就是 MC 客户端会做的事：连本机端口，说话，听回话
    let mut player = TcpStream::connect(local).await.expect("连本机端口");
    player.write_all(b"knock knock").await.expect("写");
    let mut back = [0u8; 11];
    player.read_exact(&mut back).await.expect("读");
    assert_eq!(&back, b"knock knock");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_connection_also_works() {
    // MC 进来时会开好几条连接（握手、状态、登录），只通一条是不够的
    let echo = echo_server().await;

    let host = pipe::bind_endpoint().await.expect("房主端点");
    let node_addr = loopback_addr(&host);
    tokio::spawn(pipe::serve(host, echo));

    let guest = pipe::bind_endpoint().await.expect("房客端点");
    let local = pipe::listen_local(guest, node_addr).await.expect("本机端口");

    for round in 0..3u8 {
        let mut player = TcpStream::connect(local).await.expect("连本机端口");
        let payload = [round; 4];
        player.write_all(&payload).await.expect("写");
        let mut back = [0u8; 4];
        player.read_exact(&mut back).await.expect("读");
        assert_eq!(back, payload, "第 {round} 条连接的数据不对");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn big_payload_survives_the_trip() {
    // 一帧区块数据能有几百 KB：分片、顺序、完整性都要对
    let echo = echo_server().await;

    let host = pipe::bind_endpoint().await.expect("房主端点");
    let node_addr = loopback_addr(&host);
    tokio::spawn(pipe::serve(host, echo));

    let guest = pipe::bind_endpoint().await.expect("房客端点");
    let local = pipe::listen_local(guest, node_addr).await.expect("本机端口");

    let payload: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();

    let mut player = TcpStream::connect(local).await.expect("连本机端口");
    let expected = payload.clone();
    let writer = tokio::spawn(async move {
        player.write_all(&expected).await.expect("写");
        let mut back = vec![0u8; expected.len()];
        player.read_exact(&mut back).await.expect("读");
        back
    });

    let back = tokio::time::timeout(std::time::Duration::from_secs(30), writer)
        .await
        .expect("30 秒内应当传完")
        .expect("任务不应 panic");

    assert_eq!(back.len(), payload.len());
    assert_eq!(back, payload, "大块数据被改动了");
}
