//! `ConnState` 与 `FailReason`。
//!
//! **全项目只在这里定义一次。** 界面的文案、测试的断言、日志的字段全部引用这里的枚举，
//! 不许在别处再写一套字符串。取值本身以 `AGENTS.md` 的状态机表为唯一真相。

/// 客户端连接状态。7 个值，一个不多一个不少。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnState {
    /// 未开始
    Idle,
    /// 房主正在开门
    Creating,
    /// 门开着，等对方
    Waiting,
    /// 交换候选地址、打洞中
    Punching,
    /// P2P 直连成功
    Direct,
    /// 打洞失败，走中继
    Relay,
    /// 终止，带原因码
    Failed,
}

impl ConnState {
    pub const ALL: [ConnState; 7] = [
        ConnState::Idle,
        ConnState::Creating,
        ConnState::Waiting,
        ConnState::Punching,
        ConnState::Direct,
        ConnState::Relay,
        ConnState::Failed,
    ];

    /// 给界面/持久化用的稳定名字。改名等于改协议，别随手动。
    pub fn as_str(self) -> &'static str {
        match self {
            ConnState::Idle => "idle",
            ConnState::Creating => "creating",
            ConnState::Waiting => "waiting",
            ConnState::Punching => "punching",
            ConnState::Direct => "direct",
            ConnState::Relay => "relay",
            ConnState::Failed => "failed",
        }
    }
}

/// 失败原因码。只在 `ConnState::Failed` 上出现。
///
/// 归因规则：能归因到更具体的原因就报具体原因，`RelayUnavailable` 只作兜底
/// （打洞超时这种"没有具体归因"的情况，以及中继中途断开）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailReason {
    /// 门牌号不对，或者已经过期
    CodeNotFound,
    /// 门开着，但屋里没人（房主没开服）
    HostOffline,
    /// 门被挡住了（本机防火墙）
    BlockedByFirewall,
    /// 两边都出不去（对称 NAT）
    SymmetricNat,
    /// 绕路也走不通
    RelayUnavailable,
    /// 节点的协议版本和客户端对不上（客户端比节点新，或反过来）
    NodeOutdated,
}

impl FailReason {
    pub const ALL: [FailReason; 6] = [
        FailReason::CodeNotFound,
        FailReason::HostOffline,
        FailReason::BlockedByFirewall,
        FailReason::SymmetricNat,
        FailReason::RelayUnavailable,
        FailReason::NodeOutdated,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FailReason::CodeNotFound => "code_not_found",
            FailReason::HostOffline => "host_offline",
            FailReason::BlockedByFirewall => "blocked_by_firewall",
            FailReason::SymmetricNat => "symmetric_nat",
            FailReason::RelayUnavailable => "relay_unavailable",
            FailReason::NodeOutdated => "node_outdated",
        }
    }
}
