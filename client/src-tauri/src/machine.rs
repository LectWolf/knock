//! 状态机的唯一实现：事件进、状态出。不碰 IO，因此可以穷举测试。
//!
//! 三条贯穿全文件的规则（来自 `docs/GLOSSARY.md` §0）：
//!
//! 1. **房间与会话是两件事。** 房主开门后可以先后接待多个会话，会话结束房间通常还在。
//! 2. **会话结束不等于失败。** 房主侧回 `waiting`（门还开着），只有房客侧才进 `failed`。
//! 3. **失败码能具体就具体。** 对称 NAT / 防火墙有明确归因时不用 `relay_unavailable` 兜底。
//!
//! 另外：`RelayPolicy::Ask` 的"要不要绕路"是一个**事件**，不是第 8 个状态。

use crate::state::{ConnState, FailReason};

/// 这次连接里，本机是房主还是房客。同一套状态机，两头语义不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Guest,
}

/// 打洞失败后怎么走。这是**连接前的偏好**，所以在 `Machine::new` 时给定，不在中途改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayPolicy {
    /// 自动走中继（默认，与 AGENTS.md 一致）
    Auto,
    /// 停下来问用户「要绕路吗？」
    Ask,
    /// 绝不走中继
    Never,
}

/// 喂给状态机的事件。事件来自 transport 内部（网络、计时器、用户点击），
/// 界面不产生事件，界面只消费 [`Update`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    // —— 入口 ——
    /// 房主点了开门
    OpenRequested,
    /// 房客提交了门牌号/邀请串
    KnockSubmitted,

    // —— 节点（信令）——
    /// 节点确认房间已登记，拿到门牌号
    RoomRegistered,
    /// 节点登记失败（节点不可达/拒绝）
    RoomRegisterFailed,
    /// 有人来敲门（房主侧感知到对端出现）
    PeerKnocked,
    /// 节点上查不到这个门牌号（码错或已过期）
    CodeRejected,
    /// 节点的协议版本和客户端对不上——这事没救，只能让节点主人更新
    NodeVersionMismatch,

    // —— 打洞 ——
    PunchSucceeded,
    PunchTimedOut,
    /// 连到了对端那台机器，但它上面没有开 MC 服务端（端口是闭的）
    HostNotServing,
    /// 本机防火墙/权限拒绝，连出向 socket 都建不起来
    PunchBlocked,
    /// 判定为对称 NAT，打不通
    PunchSymmetricNat,

    // —— 中继 ——
    RelayAccepted,
    RelayRejected,
    /// 中继建立之后中途断开
    RelayDropped,

    // —— 用户决定（只有 `RelayPolicy::Ask` 会用到）——
    RelayAcceptedByUser,
    RelayDeclinedByUser,

    // —— 结束 ——
    /// 对端断开（正常离开或掉线）
    PeerLeft,
    /// 本端关门
    CloseRequested,
}

/// 状态机对外的唯一输出。界面渲染它。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Update {
    pub state: ConnState,
    /// 只有 `Failed` 时非空
    pub fail: Option<FailReason>,
    /// 是否需要用户当场决定「要不要绕路」。**这是一个事件标记，不是状态**：
    /// 此时 `state` 仍然是 `Punching`。
    pub relay_offer: bool,
}

impl Update {
    pub(crate) fn new(state: ConnState, fail: Option<FailReason>, relay_offer: bool) -> Self {
        Self { state, fail, relay_offer }
    }
}

/// 在某个状态下收到一个不该出现的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalEvent {
    pub from: ConnState,
    pub event: Event,
}

/// 状态迁移是否合法。测试里有一张完整的 7×7 表来锁死它。
pub fn legal(from: ConnState, to: ConnState) -> bool {
    use ConnState::*;
    matches!(
        (from, to),
        (Idle, Creating)
            | (Idle, Punching)
            | (Creating, Waiting)
            | (Creating, Failed)
            | (Creating, Idle)
            | (Waiting, Punching)
            | (Waiting, Idle)
            | (Punching, Punching)
            | (Punching, Direct)
            | (Punching, Relay)
            | (Punching, Failed)
            | (Punching, Idle)
            | (Direct, Waiting)
            | (Direct, Idle)
            | (Direct, Failed)
            | (Relay, Waiting)
            | (Relay, Idle)
            | (Relay, Failed)
            | (Failed, Idle)
            | (Failed, Creating)
            | (Failed, Punching)
    )
}

pub struct Machine {
    role: Role,
    policy: RelayPolicy,
    state: ConnState,
    fail: Option<FailReason>,
    /// 打洞为什么失败。用于"中继也不成"时的归因：具体原因优先于 relay_unavailable。
    punch_failure: Option<FailReason>,
    /// `RelayPolicy::Ask` 下是否已经抛出待决策的 offer。
    offering: bool,
}

impl Machine {
    pub fn new(role: Role, policy: RelayPolicy) -> Self {
        Self {
            role,
            policy,
            state: ConnState::Idle,
            fail: None,
            punch_failure: None,
            offering: false,
        }
    }

    /// 现在是房主还是房客。关门时要靠它决定"要不要通知节点把房间收了"。
    pub fn role(&self) -> Role {
        self.role
    }

    pub fn state(&self) -> ConnState {
        self.state
    }

    pub fn fail(&self) -> Option<FailReason> {
        self.fail
    }

    pub fn apply(&mut self, event: Event) -> Result<Update, IllegalEvent> {
        let from = self.state;
        let next = self
            .plan(event)
            .ok_or(IllegalEvent { from, event })?;

        debug_assert!(legal(from, next.state), "非法迁移 {from:?} -> {:?}", next.state);
        self.state = next.state;
        self.fail = next.fail;
        self.offering = next.relay_offer;
        Ok(next)
    }

    /// 迁移计划。返回 `None` 表示这个状态下不该收到这个事件。
    fn plan(&mut self, event: Event) -> Option<Update> {
        use ConnState::*;
        use Event as E;
        let from = self.state;

        let plain = |to| Some(Update::new(to, None, false));
        let dead = |why| Some(Update::new(Failed, Some(why), false));
        let offer = || Some(Update::new(Punching, None, true));

        match event {
            // —— 入口：房主进 creating，房客直接进 punching ——
            // 房客没有"正在查门牌"这个状态，查码发生在 punching 内部（AGENTS.md）。
            E::OpenRequested if from == Idle || from == Failed => {
                self.punch_failure = None;
                plain(Creating)
            }
            E::KnockSubmitted if from == Idle || from == Failed => {
                self.punch_failure = None;
                plain(Punching)
            }

            // —— 信令 ——
            E::RoomRegistered if from == Creating => plain(Waiting),
            E::RoomRegisterFailed if from == Creating => dead(FailReason::RelayUnavailable),
            E::PeerKnocked if from == Waiting => plain(Punching),
            E::CodeRejected if from == Punching => dead(FailReason::CodeNotFound),
            // 版本对不上在开门和敲门两条路上都可能发生，两个"进行中"状态都要收
            E::NodeVersionMismatch if from == Punching || from == Creating => {
                dead(FailReason::NodeOutdated)
            }

            // —— 打洞 ——
            E::PunchSucceeded if from == Punching => plain(Direct),
            E::HostNotServing if from == Punching => dead(FailReason::HostOffline),
            E::PunchBlocked if from == Punching => dead(FailReason::BlockedByFirewall),
            E::PunchSymmetricNat if from == Punching => {
                self.punch_failure = Some(FailReason::SymmetricNat);
                match self.policy {
                    RelayPolicy::Never => dead(FailReason::SymmetricNat),
                    RelayPolicy::Auto => plain(Punching), // 留在 punching，接下来试中继
                    RelayPolicy::Ask => offer(),
                }
            }
            E::PunchTimedOut if from == Punching => {
                // 超时没有具体归因可给，所以兜底码是 relay_unavailable
                self.punch_failure = None;
                match self.policy {
                    RelayPolicy::Never => dead(FailReason::RelayUnavailable),
                    RelayPolicy::Auto => plain(Punching),
                    RelayPolicy::Ask => offer(),
                }
            }

            // —— 中继 ——
            E::RelayAccepted if from == Punching => plain(Relay),
            E::RelayRejected if from == Punching => {
                dead(self.punch_failure.unwrap_or(FailReason::RelayUnavailable))
            }
            E::RelayAcceptedByUser if from == Punching && self.offering => plain(Relay),
            E::RelayDeclinedByUser if from == Punching && self.offering => {
                dead(self.punch_failure.unwrap_or(FailReason::RelayUnavailable))
            }

            // —— 会话结束：房主回 waiting，房客进 failed ——
            E::RelayDropped if from == Relay => match self.role {
                Role::Host => plain(Waiting),
                Role::Guest => dead(FailReason::RelayUnavailable),
            },
            E::PeerLeft if from == Direct || from == Relay => match self.role {
                Role::Host => plain(Waiting),
                Role::Guest => dead(FailReason::HostOffline),
            },

            // —— 关门：任何非 idle 状态都能回 idle，且这不是失败 ——
            E::CloseRequested if from != Idle => {
                self.punch_failure = None;
                plain(Idle)
            }

            _ => None,
        }
    }
}
