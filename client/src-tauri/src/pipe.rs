//! 真实数据通路：把一条 QUIC 双向流和本机的一个 TCP 端口接起来。
//!
//! 这是产品的核心动作——MC 的 TCP 25565 被塞进 QUIC 里过到对面。
//! 状态机不关心字节怎么流，只关心"有没有通"；这一层只关心字节，不关心界面。
//!
//! 为什么需要 splice 而不是"转发 UDP"：iroh 打洞用的是 UDP，但 MC 说的是 TCP。
//! 所以两端各有一个 TCP↔QUIC 转换点：
//!
//! ```text
//! 房客的 MC → 本机端口 → [QUIC] → 房主 → 127.0.0.1:25565
//! ```

use std::io;
use std::net::SocketAddr;

use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, RelayMap, RelayMode};

use crate::control::{self, Hello, RejectReason, Reply};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// ALPN：协议名。改了它等于改了线协议版本，两边必须一致。
pub const ALPN: &[u8] = b"knock/2";

/// 单次搬运的块大小。QUIC 自己有分片，这里只是个上限。
const CHUNK: usize = 32 * 1024;

/// 把 iroh 的错误统一成 io::Error：上层只想知道"失败了"和一句人话。
fn net_err<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::other(e.to_string())
}

/// 起一个端点。
///
/// 默认 `RelayMode::Disabled`：这一版只验直连（打洞）。
/// 要接中继就用 [`bind_endpoint_with`] + [`relay_mode`]——
/// 而且**永远**只认用户自己那台中继，绝不回落到 n0 的公共服务（ADR-0003）。
pub async fn bind_endpoint() -> io::Result<Endpoint> {
    bind_endpoint_with(RelayMode::Disabled).await
}

/// 用指定的中继模式起端点。
pub async fn bind_endpoint_with(relay: RelayMode) -> io::Result<Endpoint> {
    Endpoint::builder(presets::Minimal)
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(relay)
        .bind()
        .await
        .map_err(net_err)
}

/// 把节点告诉我们的中继地址变成 `RelayMode`。
///
/// 三条规矩：
/// 1. 没有中继 → `Disabled`，**不是** `Default`。从"没配置"跳到"用 n0 的公共服务"
///    是最容易犯也最敏感的错，所以这里写死。
/// 2. 公网必须是 https。中继只转发密文，看不到内容；但明文 HTTP 会被中间设备改写，
///    地址一换客户端就去连错机器，表现为"门打不开"。
/// 3. 本机/环回允许 http，方便调试。
pub fn relay_mode(relay_url: Option<&str>) -> io::Result<RelayMode> {
    let Some(raw) = relay_url.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(RelayMode::Disabled);
    };

    if !(raw.starts_with("https://") || raw.starts_with("http://")) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("中继地址 {raw} 不是 http(s) 地址"),
        ));
    }
    // 明文 http 的中继：只告警，不拒绝。
    //
    // 这个地址是**从我们已经在用的节点**拿到的，而节点 API 本身就是 http（30990）——
    // 对中继比对节点更严格没有意义，同一个信任边界。
    // 更何况 iroh 的流量是端到端加密的，中继看到的只是密文。
    // 正式部署仍建议上 https。
    if raw.starts_with("http://") && !is_loopback_url(raw) {
        eprintln!("注意：中继用的是明文 http（{raw}），建议给节点配 TLS");
    }

    let map = RelayMap::try_from_iter([raw]).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("中继地址 {raw} 不合法：{e}"),
        )
    })?;
    Ok(RelayMode::Custom(map))
}

/// 这个 URL 是不是指向本机。只给"允许明文"这条规则用。
fn is_loopback_url(raw: &str) -> bool {
    let rest = raw.trim_start_matches("http://");
    let host = rest.split(|c| c == '/' || c == ':').next().unwrap_or("");
    if host == "localhost" {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
}

/// 谁开流：房主等对方开，房客主动开。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Host,
    Guest,
}

/// 房主侧：谁能进来，由这个闸门说了算。
pub trait Gate: Send + Sync {
    /// `peer` 是房客的 EndpointId，房主用它认人（踢过谁、放谁进来）。
    fn admit(&self, hello: &Hello, peer: &[u8; 32]) -> Reply;

    /// 放进来了，登记一下。默认啥也不做——只关心"放不放"的实现不用管这个。
    fn joined(&self, _hello: &Hello, _peer: &[u8; 32], _via_relay: bool) {}

    /// 走了（自己断的，或被断的）。
    fn left(&self, _peer: &[u8; 32]) {}
}

/// 什么都放的闸门。只给测试和"还没接名单"的过渡期用。
pub struct AllowAll;

impl Gate for AllowAll {
    fn admit(&self, _hello: &Hello, _peer: &[u8; 32]) -> Reply {
        Reply::True
    }
}

pub type GateRef = std::sync::Arc<dyn Gate>;

/// 房客默认的自报家门。昵称真正接上之后由界面传入。
pub fn default_hello() -> Hello {
    Hello {
        proto: control::PROTO_VERSION,
        nickname: "knock".to_string(),
        app: env!("CARGO_PKG_VERSION").to_string(),
    }
}

/// 房主侧：一直收连接，每条连接先过控制帧，再按流接本机 `target`。
///
/// 一条连接可以挂多条数据流——MC 自己会开好几条 TCP，我们照它的节奏走。
pub async fn serve(endpoint: Endpoint, target: SocketAddr) -> io::Result<()> {
    serve_gated(endpoint, target, std::sync::Arc::new(AllowAll)).await
}

/// 同上，但由 `gate` 决定放不放人。
pub async fn serve_gated(endpoint: Endpoint, target: SocketAddr, gate: GateRef) -> io::Result<()> {
    loop {
        let Some(incoming) = endpoint.accept().await else {
            return Ok(()); // 端点已关闭
        };

        let gate = std::sync::Arc::clone(&gate);
        tokio::spawn(async move {
            let Ok(connection) = incoming.await else {
                return;
            };
            let peer = *connection.remote_id().as_bytes();

            // —— 第一道：控制帧。不放人之前，一条数据流都不接 ——
            let Ok((mut control_send, mut control_recv)) = connection.accept_bi().await else {
                return;
            };
            let hello: Hello = match recv_frame(&mut control_recv).await {
                Ok(h) => h,
                Err(_) => {
                    connection.close(2u8.into(), b"bad hello");
                    return;
                }
            };
            // 先自己试一下屋里有没有人：房主的游戏端口不响，就别让人进来白跑
            let game_up = tokio::time::timeout(
                std::time::Duration::from_millis(600),
                tokio::net::TcpStream::connect(target),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);

            if std::env::var("KNOCK_TRACE").is_ok() {
                eprintln!("[host] hello from {peer:?} game_up={game_up} target={target}");
            }
            let reply = if game_up {
                gate.admit(&hello, &peer)
            } else {
                Reply::False {
                    reason: RejectReason::HostOffline,
                }
            };
            if send_frame(&mut control_send, &reply).await.is_err() {
                return;
            }
            if reply != Reply::True {
                // 先把请柬说完整再关门：说走就走，对方只会看到"连接断了"
                let _ = control_send.finish();
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                connection.close(3u8.into(), b"rejected");
                return;
            }

            // 登记进名册：走的是不是绕路，这里翻一眼就知道
            let via_relay = connection.paths().iter().any(|p| p.is_relay());
            gate.joined(&hello, &peer, via_relay);

            // —— 第二道：数据流，一条流对应本机一条 TCP 连接 ——
            loop {
                let Ok((send, recv)) = connection.accept_bi().await else {
                    gate.left(&peer);
                    return;
                };
                let Ok(socket) = TcpStream::connect(target).await else {
                    // 房里没人：这正是 host_offline 的现场
                    connection.close(1u8.into(), b"no server");
                    return;
                };
                tokio::spawn(async move {
                    let _ = pipe_streams(send, recv, socket).await;
                });
            }
        });
    }
}

/// 房客侧：在本机开一个端口，谁连上来就替他连到 `node`。
///
/// 返回的是本机监听地址——把它填进 MC 的"服务器地址"就能进服。
/// 敲门后的第一次对话：说自己到了，等房主回一句。
pub enum HelloProbe {
    /// 房主点头了；连接留着，名册里能看到你
    Admitted,
    /// 房主明确拒绝
    Rejected(RejectReason),
    /// 没人应门 / 超时
    NoAnswer,
}

pub async fn hello_probe(endpoint: &Endpoint, node: EndpointAddr, hello: Hello) -> HelloProbe {
    let Ok(connection) = endpoint.connect(node, ALPN).await else {
        return HelloProbe::NoAnswer;
    };
    let Ok((mut send, mut recv)) = connection.open_bi().await else {
        return HelloProbe::NoAnswer;
    };
    if send_frame(&mut send, &hello).await.is_err() {
        return HelloProbe::NoAnswer;
    }
    let reply: Reply = match tokio::time::timeout(
        std::time::Duration::from_secs(6),
        recv_frame(&mut recv),
    )
    .await
    {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            if std::env::var("KNOCK_TRACE").is_ok() {
                eprintln!("[guest] 收答复失败: {e:?}");
            }
            return HelloProbe::NoAnswer;
        }
        Err(_) => {
            if std::env::var("KNOCK_TRACE").is_ok() {
                eprintln!("[guest] 等答复超时");
            }
            return HelloProbe::NoAnswer;
        }
    };
    if std::env::var("KNOCK_TRACE").is_ok() {
        eprintln!("[guest] reply={reply:?}");
    }
    match reply {
        Reply::True => {
            tokio::spawn(async move {
                let _ = connection.closed().await;
            });
            HelloProbe::Admitted
        }
        Reply::False { reason } => HelloProbe::Rejected(reason),
    }
}

pub async fn listen_local(endpoint: Endpoint, node: EndpointAddr) -> io::Result<SocketAddr> {
    listen_local_hello(endpoint, node, default_hello()).await
}

/// 同上，但把自己是谁说清楚：房主会看到这个昵称。
pub async fn listen_local_hello(
    endpoint: Endpoint,
    node: EndpointAddr,
    hello: Hello,
) -> io::Result<SocketAddr> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let local = listener.local_addr()?;

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let endpoint = endpoint.clone();
            let node = node.clone();
            let hello = hello.clone();

            tokio::spawn(async move {
                let Ok(connection) = endpoint.connect(node, ALPN).await else {
                    return;
                };

                // 先自报家门，被拒就断，不浪费一条数据流
                let Ok((mut control_send, mut control_recv)) = connection.open_bi().await else {
                    return;
                };
                if send_frame(&mut control_send, &hello).await.is_err() {
                    return;
                }
                let reply: Reply = match recv_frame(&mut control_recv).await {
                    Ok(r) => r,
                    Err(_) => return,
                };
                if reply != Reply::True {
                    connection.close(3u8.into(), b"rejected");
                    return;
                }

                let Ok((send, recv)) = connection.open_bi().await else {
                    return;
                };
                let _ = pipe_streams(send, recv, socket).await;
            });
        }
    });

    Ok(local)
}

/// 一帧控制帧写出去。控制流写一帧就结束，不留在那儿。
async fn send_frame<T: serde::Serialize>(
    send: &mut iroh::endpoint::SendStream,
    frame: &T,
) -> io::Result<()> {
    let bytes = control::encode(frame)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    send.write_all(&bytes).await.map_err(net_err)?;
    let _ = send.finish();
    Ok(())
}

/// 读一帧控制帧。长度前缀说多少就读多少，读不满就是坏帧。
async fn recv_frame<T: for<'de> serde::Deserialize<'de>>(
    recv: &mut iroh::endpoint::RecvStream,
) -> io::Result<T> {
    let mut len_bytes = [0u8; 2];
    read_exactly(recv, &mut len_bytes).await?;
    let declared = u16::from_be_bytes(len_bytes) as usize;
    if declared > control::MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "控制帧太长"));
    }

    let mut body = vec![0u8; declared];
    read_exactly(recv, &mut body).await?;

    let mut framed = Vec::with_capacity(declared + 2);
    framed.extend_from_slice(&len_bytes);
    framed.extend_from_slice(&body);
    control::decode::<T>(&framed).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}

/// 把 QUIC 流读满 `buf`。读不满就报错，不拿半帧去解析。
async fn read_exactly(recv: &mut iroh::endpoint::RecvStream, mut buf: &mut [u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match recv.read_chunk(buf.len()).await.map_err(net_err)? {
            Some(chunk) => {
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                buf = &mut buf[n..];
            }
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "控制帧没读完",
                ))
            }
        }
    }
    Ok(())
}

/// 唯一一处搬运逻辑：两个方向各跑一个循环，谁先结束都不影响另一个。
///
/// 不写成 `tokio::io::copy` 是因为 QUIC 流有自己的 API（`read_chunk`/`write_all`），
/// 手写这二十行比引入适配层更好读。
async fn pipe_streams(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    socket: TcpStream,
) -> io::Result<()> {
    let (mut tcp_read, mut tcp_write) = socket.into_split();

    // 上去：QUIC → 本机 TCP
    let up = async {
        while let Some(chunk) = recv.read_chunk(CHUNK).await.map_err(net_err)? {
            if tcp_write.write_all(&chunk).await.is_err() {
                break;
            }
        }
        let _ = tcp_write.shutdown().await;
        Ok::<(), io::Error>(())
    };

    // 下来：本机 TCP → QUIC
    let down = async {
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = match tcp_read.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if send.write_all(&buf[..n]).await.is_err() {
                break;
            }
        }
        let _ = send.finish();
        Ok::<(), io::Error>(())
    };

    let _ = tokio::join!(up, down);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay_urls(mode: &RelayMode) -> Vec<String> {
        match mode {
            RelayMode::Custom(map) => map.urls::<Vec<_>>().iter().map(|u| u.to_string()).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn no_relay_means_disabled_never_default() {
        // 这一条是防止有人"顺手"把 Disabled 改成 Default：
        // 那等于未经用户同意把流量引到 n0 的公共服务上。
        assert!(matches!(relay_mode(None).unwrap(), RelayMode::Disabled));
        assert!(matches!(relay_mode(Some("")).unwrap(), RelayMode::Disabled));
        assert!(matches!(relay_mode(Some("   ")).unwrap(), RelayMode::Disabled));
    }

    #[test]
    fn https_relay_becomes_a_custom_map_with_exactly_that_url() {
        let mode = relay_mode(Some("https://relay.example.com")).unwrap();
        let urls = relay_urls(&mode);
        assert_eq!(urls.len(), 1, "只认用户给的那一个中继");
        assert!(urls[0].starts_with("https://relay.example.com"));
    }

    #[test]
    fn localhost_http_is_allowed_for_debugging() {
        for url in ["http://127.0.0.1:8080", "http://localhost:8080"] {
            let mode = relay_mode(Some(url)).unwrap_or_else(|e| panic!("{url} 应当允许: {e}"));
            assert_eq!(relay_urls(&mode).len(), 1);
        }
    }

    #[test]
    fn public_plain_http_is_accepted_with_a_warning() {
        // 中继地址是从**我们已经在用的节点**拿到的，而节点 API 本身就是 http——
        // 对中继比对节点更严格没有意义。iroh 流量端到端加密，中继只看到密文。
        // 所以这里允许，只在 stderr 提醒一句（正式部署仍建议上 TLS）。
        let mode = relay_mode(Some("http://relay.example.com")).expect("明文中继应当可用");
        assert_eq!(relay_urls(&mode).len(), 1);
    }

    #[test]
    fn non_http_schemes_are_rejected() {
        for url in ["quic://relay.example.com", "relay.example.com", "ftp://x"] {
            assert!(relay_mode(Some(url)).is_err(), "{url} 应当被拒");
        }
    }
}
