//! 状态机规格测试。
//!
//! 读这一个文件就应该能回答两个问题：
//!
//! 1. **7 个状态按什么顺序走？** —— 每个测试都用 `assert_path` 把整条路径摊开写。
//! 2. **每个失败码在什么条件下产生？** —— 每个失败路径都断言**具体的 `FailReason`**，
//!    绝不只断言"报错了"。
//!
//! 场景与 `docs/GLOSSARY.md` §5 的自检问题一一对应（注释里标了 Q 编号）。

use std::sync::{Arc, Mutex};

use knock::machine::legal;
use knock::ConnState::{self, *};
use knock::Event::*;
use knock::FailReason::*;
use knock::{FakeTransport, RelayPolicy, Target, Transport, TransportError, Update};

// ————————————————————————————————————————————————
// 搭场子的工具
// ————————————————————————————————————————————————

fn scene(target: Target<'_>, script: Vec<knock::Event>, policy: RelayPolicy) -> FakeTransport {
    let mut t = FakeTransport::new(script).with_policy(policy);
    t.connect(target).expect("剧本必须全是合法事件");
    t
}

fn host_scene(script: Vec<knock::Event>) -> FakeTransport {
    scene(Target::Open { port: 25565 }, script, RelayPolicy::Auto)
}

fn guest_scene(script: Vec<knock::Event>) -> FakeTransport {
    scene(Target::Knock { invite: "A7K2M9" }, script, RelayPolicy::Auto)
}

fn assert_path(t: &FakeTransport, want: &[ConnState]) {
    assert_eq!(t.states(), want, "\n实际路径: {:?}\n期望路径: {:?}", t.states(), want);
}

// ————————————————————————————————————————————————
// 一、7 个状态怎么走（每条都是完整路径，不是零散断言）
// ————————————————————————————————————————————————

/// 房主开门：`idle → creating → waiting`。
/// 房主停在这里等人敲门，此时**有房间、没会话**。
#[test]
fn host_opens_door() {
    let t = host_scene(vec![RoomRegistered]);
    assert_path(&t, &[Idle, Creating, Waiting]);
    assert_eq!(t.fail(), None);
}

/// 房主接待一个成功直连的客人：`idle → creating → waiting → punching → direct`。
#[test]
fn host_serves_guest_direct() {
    let t = host_scene(vec![RoomRegistered, PeerKnocked, PunchSucceeded]);
    assert_path(&t, &[Idle, Creating, Waiting, Punching, Direct]);
    assert_eq!(t.fail(), None);
}

/// 房客敲门到直连成功：`idle → punching → direct`。
/// **注意房客不经过 `creating` / `waiting`** —— 那是房主的路径；
/// 房客也没有"正在查门牌"这个状态，查码发生在 `punching` 内部。
#[test]
fn guest_knocks_and_reaches_direct() {
    let t = guest_scene(vec![PunchSucceeded]);
    assert_path(&t, &[Idle, Punching, Direct]);
}

/// 7 个状态全都真实可达（不是"枚举里有"，而是"路径能走到"）。
#[test]
fn every_state_is_reachable() {
    let mut seen = std::collections::HashSet::new();
    for t in [
        host_scene(vec![RoomRegistered, PeerKnocked, PunchSucceeded]),
        guest_scene(vec![PunchTimedOut, RelayAccepted]),
        guest_scene(vec![CodeRejected]),
    ] {
        seen.extend(t.states());
    }
    for state in ConnState::ALL {
        assert!(seen.contains(&state), "{state:?} 没有任何路径能走到");
    }
}

// ————————————————————————————————————————————————
// 二、打洞超时降级（核心路径）
// ————————————————————————————————————————————————

/// 打洞超时 → 自动走中继 → `relay`。**这是降级，不是失败**：`fail` 必须是 `None`。
#[test]
fn punch_timeout_degrades_to_relay() {
    let t = guest_scene(vec![PunchTimedOut, RelayAccepted]);
    assert_path(&t, &[Idle, Punching, Punching, Relay]);
    assert_eq!(t.fail(), None, "绕路成功不是失败");
}

/// 打洞超时 + 中继也拒绝 → `failed(relay_unavailable)`。
/// 超时没有具体归因可给，所以只能用兜底码。
#[test]
fn punch_timeout_with_relay_rejected_reports_relay_unavailable() {
    let t = guest_scene(vec![PunchTimedOut, RelayRejected]);
    assert_path(&t, &[Idle, Punching, Punching, Failed]);
    assert_eq!(t.fail(), Some(RelayUnavailable));
}

/// 偏好 = 从不中转：打洞超时直接失败，不进 `relay`。
#[test]
fn never_policy_on_timeout_fails_without_relay() {
    let t = scene(Target::Knock { invite: "A7K2M9" }, vec![PunchTimedOut], RelayPolicy::Never);
    assert_path(&t, &[Idle, Punching, Failed]);
    assert_eq!(t.fail(), Some(RelayUnavailable));
    assert!(!t.states().contains(&Relay), "「从不中转」绝不能出现 relay");
}

// ————————————————————————————————————————————————
// 三、中继断开 / 对端掉线：两侧语义不同（Q1、Q4）
// ————————————————————————————————————————————————

/// 中继中途断开。**房客侧才报失败，房主侧回到 waiting，因为门还开着。**
/// 这条最容易写成"两边都 failed"。
#[test]
fn relay_drop_fails_guest_but_host_returns_to_waiting() {
    let guest = guest_scene(vec![PunchTimedOut, RelayAccepted, RelayDropped]);
    assert_path(&guest, &[Idle, Punching, Punching, Relay, Failed]);
    assert_eq!(guest.fail(), Some(RelayUnavailable));

    let host = host_scene(vec![RoomRegistered, PeerKnocked, PunchTimedOut, RelayAccepted, RelayDropped]);
    assert_path(&host, &[Idle, Creating, Waiting, Punching, Punching, Relay, Waiting]);
    assert_eq!(host.fail(), None, "房主的门还开着，会话结束不等于失败");
}

/// 直连状态下对端掉线。房客：`failed(host_offline)`；房主：回 `waiting`，门牌号不变（Q1）。
#[test]
fn peer_leaves_fails_guest_but_host_keeps_door_open() {
    let guest = guest_scene(vec![PunchSucceeded, PeerLeft]);
    assert_path(&guest, &[Idle, Punching, Direct, Failed]);
    assert_eq!(guest.fail(), Some(HostOffline));

    let host = host_scene(vec![RoomRegistered, PeerKnocked, PunchSucceeded, PeerLeft]);
    assert_path(&host, &[Idle, Creating, Waiting, Punching, Direct, Waiting]);
    assert_eq!(host.fail(), None);
}

// ————————————————————————————————————————————————
// 四、每个失败码的产生条件（一个测试对一种码，断言到底）
// ————————————————————————————————————————————————

/// `code_not_found`：节点上查不到这个门牌号。
/// 终态，**不许再试着绕路**——码不对，绕多远都没用。
#[test]
fn code_not_found_is_terminal() {
    let t = guest_scene(vec![CodeRejected]);
    assert_path(&t, &[Idle, Punching, Failed]);
    assert_eq!(t.fail(), Some(CodeNotFound));
    assert!(!t.states().contains(&Relay), "码不对不该去试中继");
}

/// `host_offline`：门开着，但屋里没人——房主没开 MC 服务端。
#[test]
fn host_not_serving_reports_host_offline() {
    let t = guest_scene(vec![HostNotServing]);
    assert_path(&t, &[Idle, Punching, Failed]);
    assert_eq!(t.fail(), Some(HostOffline));
}

/// `blocked_by_firewall`：本机防火墙直接挡住，连 socket 都出不去。
/// **终态且不尝试中继**——本机被挡，换条路也一样出不去。
#[test]
fn local_firewall_block_is_terminal() {
    let t = guest_scene(vec![PunchBlocked]);
    assert_path(&t, &[Idle, Punching, Failed]);
    assert_eq!(t.fail(), Some(BlockedByFirewall));
    assert!(!t.states().contains(&Relay));
}

/// `symmetric_nat`：有具体归因时**必须优先于**兜底码。
/// 同样是"中继也拒绝"，但打洞的失败原因是对称 NAT，所以报的具体原因是它。
#[test]
fn symmetric_nat_wins_over_relay_unavailable() {
    let t = guest_scene(vec![PunchSymmetricNat, RelayRejected]);
    assert_path(&t, &[Idle, Punching, Punching, Failed]);
    assert_eq!(
        t.fail(),
        Some(SymmetricNat),
        "能归因到对称 NAT 时不能用 relay_unavailable 兜底"
    );
}

/// 同样的输入，偏好改成「从不中转」：立刻报对称 NAT，不进 `relay`。
#[test]
fn symmetric_nat_with_never_policy_fails_immediately() {
    let t = scene(Target::Knock { invite: "A7K2M9" }, vec![PunchSymmetricNat], RelayPolicy::Never);
    assert_path(&t, &[Idle, Punching, Failed]);
    assert_eq!(t.fail(), Some(SymmetricNat));
}

/// `relay_unavailable`：中继建立不了（节点不允许中转 / 满了 / 不可达）。
#[test]
fn relay_rejected_reports_relay_unavailable() {
    let t = guest_scene(vec![RelayRejected]);
    assert_eq!(t.fail(), Some(RelayUnavailable));
}

/// 5 个失败码全部被真实路径产生过一遍。加新码时这个测试会提醒你补路径。
#[test]
fn every_fail_reason_has_a_path() {
    let cases: Vec<(FakeTransport, knock::FailReason)> = vec![
        (guest_scene(vec![CodeRejected]), CodeNotFound),
        (guest_scene(vec![HostNotServing]), HostOffline),
        (guest_scene(vec![PunchBlocked]), BlockedByFirewall),
        (guest_scene(vec![PunchSymmetricNat, RelayRejected]), SymmetricNat),
        (guest_scene(vec![PunchTimedOut, RelayRejected]), RelayUnavailable),
    ];
    for (t, want) in cases {
        assert_eq!(t.fail(), Some(want), "{want:?} 的产生路径断了");
    }
}

// ————————————————————————————————————————————————
// 五、「要不要绕路」是事件不是状态（Q：7 态不能被撑成 8 态）
// ————————————————————————————————————————————————

/// 偏好 = 每次询问：打洞超时后**停在 `punching`**，抛一个 `relay_offer`，
/// 状态值本身没有变化。用户拒绝 → `failed(relay_unavailable)`。
#[test]
fn ask_policy_pauses_in_punching_then_declines() {
    let mut t = scene(Target::Knock { invite: "A7K2M9" }, vec![PunchTimedOut], RelayPolicy::Ask);
    assert_path(&t, &[Idle, Punching, Punching]);
    assert!(t.updates().last().expect("有更新").relay_offer, "应当抛出待决策的 offer");

    t.feed(RelayDeclinedByUser).expect("拒绝是合法事件");
    assert_path(&t, &[Idle, Punching, Punching, Failed]);
    assert_eq!(t.fail(), Some(RelayUnavailable));
}

/// 偏好 = 每次询问，用户同意 → 进 `relay`。
#[test]
fn ask_policy_accepts_into_relay() {
    let mut t = scene(Target::Knock { invite: "A7K2M9" }, vec![PunchTimedOut], RelayPolicy::Ask);
    t.feed(RelayAcceptedByUser).expect("同意是合法事件");
    assert_path(&t, &[Idle, Punching, Punching, Relay]);
    assert_eq!(t.fail(), None);
}

// ————————————————————————————————————————————————
// 六、正常结束不是失败（Q5）
// ————————————————————————————————————————————————

/// 房主把门关上：回 `idle`，**不是 `failed`**。
#[test]
fn closing_the_door_is_not_a_failure() {
    let mut t = host_scene(vec![RoomRegistered]);
    t.close();
    assert_path(&t, &[Idle, Creating, Waiting, Idle]);
    assert_eq!(t.fail(), None);
}

/// 没连上就 close 也不报错（幂等）。
#[test]
fn close_before_connect_is_a_noop() {
    let mut t = FakeTransport::new(vec![]);
    t.close();
    assert_eq!(t.state(), Idle);
}

/// 失败之后可以直接再敲一次（`failed → punching`），不用先回 idle。
#[test]
fn retry_from_failed_goes_straight_to_punching() {
    let t = guest_scene(vec![CodeRejected, KnockSubmitted, PunchSucceeded]);
    assert_path(&t, &[Idle, Punching, Failed, Punching, Direct]);
    assert_eq!(t.fail(), None, "重试成功之后不该还挂着旧的失败码");
}

// ————————————————————————————————————————————————
// 七、接口契约本身
// ————————————————————————————————————————————————

/// 迁移表被完整锁死：7×7 里只有这些组合合法。
/// 增删任何一条 = 改状态机 = 必须同时改这里和 AGENTS.md。
#[test]
fn transition_table_is_locked() {
    let allowed: &[(ConnState, ConnState)] = &[
        // 入口
        (Idle, Creating),
        (Idle, Punching),
        // 房主开门
        (Creating, Waiting),
        (Creating, Failed),
        (Creating, Idle),
        // 等人来敲 / 关门
        (Waiting, Punching),
        (Waiting, Idle),
        // 打洞：成功、降级（留在 punching 试中继）、失败、放弃
        (Punching, Punching),
        (Punching, Direct),
        (Punching, Relay),
        (Punching, Failed),
        (Punching, Idle),
        // 直连之后：对端走了（房主回 waiting）、关门、房客侧失败
        (Direct, Waiting),
        (Direct, Idle),
        (Direct, Failed),
        // 中继之后：同上
        (Relay, Waiting),
        (Relay, Idle),
        (Relay, Failed),
        // 失败之后：回 idle、重新开门、再敲一次
        (Failed, Idle),
        (Failed, Creating),
        (Failed, Punching),
    ];

    for from in ConnState::ALL {
        for to in ConnState::ALL {
            let want = allowed.contains(&(from, to));
            assert_eq!(legal(from, to), want, "迁移表不一致：{from:?} -> {to:?}");
        }
    }
}

/// `on_state` 订阅者收到的流，必须与 transport 内部记录完全一致。
/// 这是"界面只渲染推送来的状态"这个契约的地基。
#[test]
fn subscriber_receives_the_whole_stream() {
    let seen = Arc::new(Mutex::new(Vec::<Update>::new()));
    let sink_seen = Arc::clone(&seen);

    let mut t = FakeTransport::new(vec![RoomRegistered]);
    t.on_state(Box::new(move |u: &Update| sink_seen.lock().expect("锁").push(*u)));
    t.connect(Target::Open { port: 25565 }).expect("开门应当受理");

    assert_eq!(seen.lock().expect("锁").as_slice(), t.updates());
    assert_eq!(t.state(), Waiting);
}

/// 当前状态下不该出现的事件，必须被明确拒绝，而不是悄悄吃掉。
#[test]
fn illegal_event_is_rejected() {
    let mut t = FakeTransport::new(vec![]);
    t.connect(Target::Knock { invite: "A7K2M9" }).expect("敲门的入口事件合法");

    // 还在 punching（没有会话）就收到"对端走了"，属于状态机 bug，必须报错。
    match t.feed(PeerLeft) {
        Err(TransportError::IllegalEvent(e)) => assert_eq!(e.from, Punching),
        other => panic!("应当拒绝非法事件，实际: {other:?}"),
    }

    // 新 transport 不能直接 feed
    let mut fresh = FakeTransport::new(vec![]);
    assert_eq!(fresh.feed(PeerLeft), Err(TransportError::NotConnected));
}

/// 节点版本对不上：这是**没有退路**的失败，两条入口都得直接落到 Failed。
///
/// 这里要单独测，因为它既不是打洞失败也不是查码失败——归到 relay_unavailable
/// 会骗人去重试，而重试一百次也不会好。
#[test]
fn a_version_mismatch_kills_either_path() {
    let guest = guest_scene(vec![NodeVersionMismatch]);
    assert_eq!(guest.state(), Failed);
    assert_eq!(guest.fail(), Some(NodeOutdated));

    let host = host_scene(vec![NodeVersionMismatch]);
    assert_eq!(host.state(), Failed);
    assert_eq!(host.fail(), Some(NodeOutdated));
}
