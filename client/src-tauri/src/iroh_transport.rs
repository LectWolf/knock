//! 真实的传输实现：把房间接口、路径策略、iroh 数据通路和状态机缝在一起。
//!
//! 这是"敲门"真正发生的地方。界面只看到一个 [`Transport`]：
//! 调 `connect`，然后消费状态回调。
//!
//! 路径规则（ADR-0003「一次决定，不中途换轨」）：
//! 先试直连（端点不带中继），打不通才重新绑定带中继的端点。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iroh::{Endpoint, EndpointAddr, PublicKey, RelayUrl};

use crate::invite::Invite;
use crate::machine::{Event, Machine, RelayPolicy, Role, Update};
use crate::pipe;
use crate::policy::{self, AttemptKind};
use crate::resolve;
use crate::room::{RoomClient, RoomError};
use crate::sessions::SessionList;
use crate::state::{ConnState, FailReason};
use crate::transport::{StateSink, Target, Transport, TransportError};

type Sink = Arc<Mutex<Option<StateSink>>>;

/// 多久续一次命。节点那边的房间寿命是 10 分钟，60 秒留足余量。
fn announce_every() -> Duration {
    std::env::var("KNOCK_ANNOUNCE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(60))
}

/// 真实传输。内部一个后台线程跑 tokio 运行时，状态机是唯一真相。
pub struct IrohTransport {
    client: Arc<dyn RoomClient + Send + Sync>,
    machine: Arc<Mutex<Machine>>,
    sink: Sink,
    running: Arc<AtomicBool>,
    code: Arc<Mutex<Option<String>>>,
    local: Arc<Mutex<Option<SocketAddr>>>,
    policy: RelayPolicy,
    /// 房主侧的"门里的人"。房客侧用不上，但留着同一个实例，免得两套生命周期。
    sessions: Arc<SessionList>,
}

impl IrohTransport {
    /// `client` 指向用户添加的那个节点。
    pub fn new(client: Arc<dyn RoomClient + Send + Sync>) -> Self {
        Self {
            client,
            machine: Arc::new(Mutex::new(Machine::new(Role::Guest, RelayPolicy::Auto))),
            sink: Arc::new(Mutex::new(None)),
            running: Arc::new(AtomicBool::new(false)),
            code: Arc::new(Mutex::new(None)),
            local: Arc::new(Mutex::new(None)),
            policy: RelayPolicy::Auto,
            sessions: Arc::new(SessionList::new()),
        }
    }

    /// 门里的人。界面拿到的是快照，踢人走 `kick`。
    pub fn sessions(&self) -> Arc<SessionList> {
        Arc::clone(&self.sessions)
    }

    /// 设置"打洞失败怎么办"的偏好。必须在 `connect` 之前调。
    pub fn with_policy(mut self, policy: RelayPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 房主拿到的门牌号。只有进了 `waiting` 才有值。
    pub fn room_code(&self) -> Option<String> {
        self.code.lock().unwrap().clone()
    }

    /// 房客的本地入口地址：MC 客户端应当连到这里。
    pub fn local_addr(&self) -> Option<SocketAddr> {
        *self.local.lock().unwrap()
    }
}

impl Transport for IrohTransport {
    fn connect(&mut self, target: Target<'_>) -> Result<(), TransportError> {
        let role = match target {
            Target::Open { .. } => Role::Host,
            Target::Knock { .. } => Role::Guest,
        };

        // 每次 connect 都是一次全新的会话：状态机重建，不留上一次的残渣
        *self.machine.lock().unwrap() = Machine::new(role, self.policy);
        self.running.store(true, Ordering::SeqCst);

        let entry = match target {
            Target::Open { .. } => Event::OpenRequested,
            Target::Knock { .. } => Event::KnockSubmitted,
        };
        emit(&self.machine, &self.sink, entry);

        let machine = Arc::clone(&self.machine);
        let sink = Arc::clone(&self.sink);
        let running = Arc::clone(&self.running);
        let client = Arc::clone(&self.client);
        let code_slot = Arc::clone(&self.code);
        let local_slot = Arc::clone(&self.local);
        let sessions = Arc::clone(&self.sessions);
        let policy = self.policy;

        let flow = match target {
            Target::Open { port } => Flow::Open { port },
            Target::Knock { invite } => match Invite::parse(invite) {
                Ok(inv) => match inv.node().map(|n| n.pin) {
                    Some(pin) => match inv.room() {
                        Some(room) => Flow::Knock { code: room.to_string(), pin },
                        None => Flow::Hopeless(FailReason::CodeNotFound),
                    },
                    None => Flow::Hopeless(FailReason::CodeNotFound),
                },
                Err(_) => Flow::Hopeless(FailReason::CodeNotFound),
            },
        };

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("建 tokio 运行时");
            rt.block_on(async move {
                let mut ctx = Ctx {
                    machine,
                    sink,
                    running,
                    client,
                    code: code_slot,
                    local: local_slot,
                    policy,
                    sessions,
                };
                let _ = match flow {
                    Flow::Open { port } => ctx.open(port).await,
                    Flow::Knock { code, pin } => ctx.knock(code, pin).await,
                    Flow::Hopeless(reason) => {
                        ctx.fail(reason);
                        Ok(())
                    }
                };

                // 会话期间运行时必须活着：它一退出，本地入口、QUIC 连接、
                // 房主那边的 serve 全部跟着消失。
                while ctx.running.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
        });

        Ok(())
    }

    fn on_state(&mut self, sink: StateSink) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    fn close(&mut self) {
        self.running.store(false, Ordering::SeqCst);

        // 房主关门：告诉节点一声，别让房间在那边挂到自然过期。
        // 房客不能发这个——他关掉的只是自己那条通路，门是别人的。
        let is_host = matches!(self.machine.lock().unwrap().role(), Role::Host);
        if is_host {
            if let Some(code) = self.code.lock().unwrap().clone() {
                let client = Arc::clone(&self.client);
                // close() 是同步接口，别把 HTTP 往返塞进界面线程
                std::thread::spawn(move || {
                    let _ = client.close(&code);
                });
            }
        }

        let _ = emit(&self.machine, &self.sink, Event::CloseRequested);
    }

    fn state(&self) -> ConnState {
        self.machine.lock().unwrap().state()
    }
}

/// 后台线程要做的事。
enum Flow {
    Open { port: u16 },
    Knock { code: String, pin: [u8; 8] },
    Hopeless(FailReason),
}

struct Ctx {
    machine: Arc<Mutex<Machine>>,
    sink: Sink,
    running: Arc<AtomicBool>,
    client: Arc<dyn RoomClient + Send + Sync>,
    code: Arc<Mutex<Option<String>>>,
    local: Arc<Mutex<Option<SocketAddr>>>,
    policy: RelayPolicy,
    /// 房主侧的"门里的人"。房客侧用不上，但留着同一个实例，免得两套生命周期。
    sessions: Arc<SessionList>,
}

impl Ctx {
    fn alive(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    fn emit(&self, event: Event) -> Option<Update> {
        emit(&self.machine, &self.sink, event)
    }

    /// 把失败原因翻成状态机能接的事件（状态机不接受"直接设置失败"）。
    fn fail(&self, reason: FailReason) {
        let event = match reason {
            FailReason::CodeNotFound => Event::CodeRejected,
            FailReason::HostOffline => Event::HostNotServing,
            FailReason::BlockedByFirewall => Event::PunchBlocked,
            FailReason::SymmetricNat => Event::PunchSymmetricNat,
            FailReason::RelayUnavailable => Event::PunchTimedOut,
            FailReason::NodeOutdated => Event::NodeVersionMismatch,
        };
        let _ = self.emit(event);
    }

    // —— 房主：开门 ——
    async fn open(&mut self, port: u16) -> Result<(), ()> {
        let client = Arc::clone(&self.client);
        let room = match tokio::task::spawn_blocking(move || client.open(port)).await {
            Ok(Ok(r)) => r,
            _ => {
                self.emit(Event::RoomRegisterFailed);
                return Ok(());
            }
        };
        *self.code.lock().unwrap() = Some(room.code.clone());
        let token = room.token.clone().unwrap_or_default();

        // 房主一次性绑定：既要能被直连，也要能被中继找到（身份要 announce）
        let relay = pipe::relay_mode(room.relay_url.as_deref()).ok();
        let endpoint = match relay {
            Some(mode) => pipe::bind_endpoint_with(mode).await,
            None => pipe::bind_endpoint().await,
        };
        let Ok(endpoint) = endpoint else {
            self.emit(Event::RoomRegisterFailed);
            return Ok(());
        };

        self.emit(Event::RoomRegistered);

        // 把自己的身份与直连地址挂到房间上。
        // 端点的地址要等网卡探完才有，所以先等一会儿——announce 空地址会被节点拒绝。
        let host_id = hex32(endpoint.id().as_bytes());
        let mut addrs: Vec<String> = Vec::new();
        for _ in 0..50 {
            addrs = direct_addrs(&endpoint).iter().map(|a| a.to_string()).collect();
            if !addrs.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let code = room.code.clone();

        // 心跳要用的那几样，先留一份；下面那次 announce 会把原件吃掉
        let hb_code = code.clone();
        let hb_token = token.clone();
        let hb_id = host_id.clone();
        let hb_addrs = addrs.clone();

        let client = Arc::clone(&self.client);
        let announced = tokio::task::spawn_blocking(move || {
            client.announce(&code, &token, &host_id, &addrs)
        })
        .await;

        if !matches!(announced, Ok(Ok(()))) {
            // 地址挂不上去，这门就是白开的：如实报失败，别假装在等
            self.emit(Event::RoomRegisterFailed);
            return Ok(());
        }

        // 心跳：房间寿命是从**最后一次 announce** 算的，不续命十分钟后门就自己没了，
        // 而界面上还写着"门开着"——这是最伤信任的一种不一致。
        let hb_client = Arc::clone(&self.client);
        let hb_alive = Arc::clone(&self.running);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(announce_every()).await;
                if !hb_alive.load(Ordering::SeqCst) {
                    break;
                }
                let client = Arc::clone(&hb_client);
                let (code, token, id, addrs) = (
                    hb_code.clone(),
                    hb_token.clone(),
                    hb_id.clone(),
                    hb_addrs.clone(),
                );
                let sent = tokio::task::spawn_blocking(move || {
                    client.announce(&code, &token, &id, &addrs)
                })
                .await;
                // 房间已经没了（被回收/被节点拒），再刷也是空转
                if !matches!(sent, Ok(Ok(()))) {
                    break;
                }
            }
        });

        // 有人敲门就把 MC 流量接进本机服务端；谁能进来由名册说了算
        let target: SocketAddr = ([127, 0, 0, 1], port).into();
        let gate: pipe::GateRef = self.sessions.clone();
        tokio::spawn(async move {
            let _ = pipe::serve_gated(endpoint, target, gate).await;
        });
        Ok(())
    }

    // —— 房客：敲门 ——
    async fn knock(&mut self, code: String, pin: [u8; 8]) -> Result<(), ()> {
        let client = Arc::clone(&self.client);
        let lookup_code = code.clone();
        let target = match tokio::task::spawn_blocking(move || {
            resolve::resolve(client.as_ref(), &lookup_code, &pin)
        })
        .await
        {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                self.fail(fail_of(e));
                return Ok(());
            }
            Err(_) => {
                self.fail(FailReason::HostOffline);
                return Ok(());
            }
        };

        if !self.alive() {
            return Ok(());
        }

        let host_id = match parse_id(&target.host_id_hex) {
            Some(id) => id,
            None => {
                self.fail(FailReason::RelayUnavailable);
                return Ok(());
            }
        };

        // 一次决定，不中途换轨
        for attempt in policy::plan(target.has_direct(), target.relay_url.is_some()) {
            if !self.alive() {
                return Ok(());
            }

            let (relay_mode, addr) = match attempt.kind {
                AttemptKind::Direct => {
                    let mut addr = EndpointAddr::new(host_id);
                    for raw in &target.addrs {
                        if let Ok(sa) = raw.parse::<SocketAddr>() {
                            addr = addr.with_ip_addr(sa);
                        }
                    }
                    (iroh::RelayMode::Disabled, addr)
                }
                AttemptKind::Relay => {
                    let Some(url) = target.relay_url.as_deref() else {
                        continue;
                    };
                    let Ok(relay_url) = url.parse::<RelayUrl>() else {
                        continue;
                    };
                    (iroh::RelayMode::Custom(match relay_map(&url) {
                        Some(m) => m,
                        None => continue,
                    }), EndpointAddr::new(host_id).with_relay_url(relay_url))
                }
            };

            let Ok(endpoint) = pipe::bind_endpoint_with(relay_mode).await else {
                continue;
            };

            // 拨一次，看能不能真的通：只在预算内等
            let budget = Duration::from_secs(attempt.budget_secs);
            let ok = matches!(
                tokio::time::timeout(budget, probe(&endpoint, addr.clone())).await,
                Ok(true)
            );

            if !ok {
                continue;
            }

            let via_relay = attempt.kind == AttemptKind::Relay;
            let event = if via_relay { Event::RelayAccepted } else { Event::PunchSucceeded };
            self.emit(event);

            let Ok(local) = pipe::listen_local(endpoint, addr).await else {
                self.fail(FailReason::RelayUnavailable);
                return Ok(());
            };
            *self.local.lock().unwrap() = Some(local);
            return Ok(());
        }

        // 两条路都不通
        let reason = if self.policy == RelayPolicy::Never {
            FailReason::SymmetricNat
        } else {
            FailReason::RelayUnavailable
        };
        self.fail(reason);
        Ok(())
    }
}

/// 探一下路能不能通：开一条双向流再立刻关掉。
///
/// 服务端会把它接到本机 MC 端口上，得到一个"连上就断"的痕迹——
/// MC 服务端对这种连接是不理会的。用这个换"这一秒到底通没通"的判断。
async fn probe(endpoint: &Endpoint, addr: EndpointAddr) -> bool {
    let Ok(conn) = endpoint.connect(addr, pipe::ALPN).await else {
        return false;
    };
    match conn.open_bi().await {
        Ok((mut send, _recv)) => {
            let _ = send.finish();
            true
        }
        Err(_) => false,
    }
}

fn emit(machine: &Arc<Mutex<Machine>>, sink: &Sink, event: Event) -> Option<Update> {
    let update = {
        let mut m = machine.lock().unwrap();
        m.apply(event).ok()
    };
    if let Some(u) = update {
        if let Some(cb) = sink.lock().unwrap().as_mut() {
            cb(&u);
        }
    }
    update
}

/// 房间接口的失败 → 用户看到的原因码。能具体就具体。
fn fail_of(e: RoomError) -> FailReason {
    match e {
        RoomError::NotFound => FailReason::CodeNotFound,
        RoomError::HostNotReady => FailReason::HostOffline,
        RoomError::Network(_) => FailReason::HostOffline,
        RoomError::PinMismatch => FailReason::RelayUnavailable,
        // 版本对不上不是网络问题：让用户去更新，别让他反复重试
        RoomError::NodeOutdated { .. } => FailReason::NodeOutdated,
        RoomError::NodeFull => FailReason::RelayUnavailable,
        RoomError::RateLimited { .. } => FailReason::RelayUnavailable,
        RoomError::BadToken => FailReason::RelayUnavailable,
        RoomError::TooManyAnnounces => FailReason::RelayUnavailable,
        RoomError::BadRequest(_) | RoomError::Protocol(_) => FailReason::RelayUnavailable,
    }
}

fn hex32(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_id(hex: &str) -> Option<PublicKey> {
    if hex.len() != 64 {
        return None;
    }
    let mut raw = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        raw[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    PublicKey::from_bytes(&raw).ok()
}

fn relay_map(url: &str) -> Option<iroh::RelayMap> {
    iroh::RelayMap::try_from_iter([url]).ok()
}

/// 本机能直接被别人拨到的地址（不含环回）。
fn direct_addrs(endpoint: &Endpoint) -> Vec<SocketAddr> {
    // 必须带端口：节点要的是 ip:port，只给 IP 会被当成坏地址拒掉
    endpoint
        .addr()
        .ip_addrs()
        .filter(|sa| !sa.ip().is_loopback())
        .copied()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_errors_map_to_the_specific_reason() {
        assert_eq!(fail_of(RoomError::NotFound), FailReason::CodeNotFound);
        assert_eq!(fail_of(RoomError::HostNotReady), FailReason::HostOffline);
        assert_eq!(fail_of(RoomError::Network("x".into())), FailReason::HostOffline);
        // 兜底才用 relay_unavailable
        assert_eq!(fail_of(RoomError::PinMismatch), FailReason::RelayUnavailable);
    }

    #[test]
    fn host_ids_round_trip() {
        // 用真的密钥对：ed25519 公钥不是任意 32 字节都合法
        let secret = iroh::SecretKey::from_bytes(&[7u8; 32]);
        let key = *secret.public().as_bytes();
        let hex = hex32(&key);
        assert_eq!(hex.len(), 64);
        let back = parse_id(&hex).expect("能解析回来");
        assert_eq!(back.as_bytes(), &key);
    }

    #[test]
    fn malformed_host_ids_are_refused() {
        assert!(parse_id("").is_none());
        assert!(parse_id("abcd").is_none());
        assert!(parse_id(&"zz".repeat(32)).is_none());
    }
}
