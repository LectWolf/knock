//! 门里的人：房主侧唯一的会话名册。
//!
//! `ConnState` 管「门」的状态，这里管「门里的人」——两者分开，不往状态机里塞第八个状态。
//! 一扇门可以在线多人（ADR-0004），所以这是一份列表，不是单个会话。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::control::{Hello, RejectReason, Reply};
use crate::pipe::Gate;

/// EndpointId 的原始字节，用来认人。
pub type Peer = [u8; 32];

/// 踢人时用来"真的把他断掉"的一个动作。
///
/// 名册只管名单，不碰连接；把连接包成闭包挂在这里，踢的时候统一执行。
pub type Closer = std::sync::Arc<dyn Fn() + Send + Sync>;

/// 门里一个人。
#[derive(Debug, Clone)]
pub struct SessionEntry {
    /// 他自报的昵称
    pub nickname: String,
    /// 是否绕路进来的
    pub via_relay: bool,
    /// 什么时候进来的
    since: Instant,
}

impl SessionEntry {
    /// 已经进来多久（秒）。界面上显示「12 分钟」用这个。
    pub fn elapsed_secs(&self) -> u64 {
        self.since.elapsed().as_secs()
    }
}

struct Inner {
    /// 现在门里有哪些人
    live: HashMap<Peer, SessionEntry>,
    /// 每个人身上挂着几条连接。
    ///
    /// 同一个人会有两条：敲门时那条常驻连接（名册靠它在），
    /// 加上他 MC 每开一条 TCP 就又拨一条。谁先断都不能把人从名册里删掉，
    /// 只有最后一条断了才算他走了。
    conns: HashMap<Peer, usize>,
    /// 本房间踢过谁（关门即忘，不落盘）
    kicked: HashSet<Peer>,
    /// 每个人身上挂着的"怎么断开他"。踢人时逐个执行。
    closers: HashMap<Peer, Vec<Closer>>,
    /// 门是不是正在关
    closing: bool,
}

/// 房主侧的名册。所有方法都能在多个连接线程里安全调用。
pub struct SessionList {
    inner: Mutex<Inner>,
}

impl Default for SessionList {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionList {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                live: HashMap::new(),
                conns: HashMap::new(),
                kicked: HashSet::new(),
                closers: HashMap::new(),
                closing: false,
            }),
        }
    }

    /// 放人进来之后登记一下。同一个人重复登记时保留最早的时间，连接数加一。
    pub fn join(&self, peer: Peer, nickname: &str, via_relay: bool) {
        let mut inner = self.lock();
        *inner.conns.entry(peer).or_insert(0) += 1;
        inner.live.entry(peer).or_insert_with(|| SessionEntry {
            nickname: nickname.to_string(),
            via_relay,
            since: Instant::now(),
        });
    }

    /// 挂一个"断他"的动作。同一个人可能有多条连接，就挂多个。
    pub fn watch(&self, peer: &Peer, closer: Closer) {
        self.lock().closers.entry(*peer).or_default().push(closer);
    }

    /// 一条连接断了。还有别的连接挂着就只是减一，最后一条断了才算人走了。
    pub fn leave(&self, peer: &Peer) {
        let mut inner = self.lock();
        match inner.conns.get_mut(peer) {
            Some(n) if *n > 1 => *n -= 1,
            Some(_) => {
                inner.conns.remove(peer);
                inner.live.remove(peer);
                inner.closers.remove(peer);
            }
            None => {
                // 没登记过（或已经被踢）：兜底删干净，不留幽灵
                inner.live.remove(peer);
            }
        }
    }

    /// 踢出去：断他的会话，并且在本房间内记住他，别让他再敲进来。
    pub fn kick(&self, peer: &Peer) {
        // 先摘名单，再断连接：断连接会回调 leave()，那时名单里已经没他了。
        let closers: Vec<Closer> = {
            let mut inner = self.lock();
            inner.live.remove(peer);
            inner.conns.remove(peer);
            inner.kicked.insert(*peer);
            inner.closers.remove(peer).unwrap_or_default()
        };
        // 别捏着锁去关连接：关的动作可能慢，别人还要进名册
        for closer in closers {
            closer();
        }
    }

    /// 开始关门：从这一刻起谁来都不放，但门里的人还没断。
    pub fn start_closing(&self) {
        self.lock().closing = true;
    }

    /// 重新开门：清掉踢人记录——关门即忘。
    pub fn reopen(&self) {
        let mut inner = self.lock();
        inner.closing = false;
        inner.kicked.clear();
    }

    /// 门里现在有多少人。
    pub fn len(&self) -> usize {
        self.lock().live.len()
    }

    /// 门里是不是空的。空着不代表门关了。
    pub fn is_empty(&self) -> bool {
        self.lock().live.len() == 0
    }

    /// 名册快照，按进来的先后排——先来的在上头。
    pub fn snapshot(&self) -> Vec<(Peer, SessionEntry)> {
        let inner = self.lock();
        let mut list: Vec<_> = inner
            .live
            .iter()
            .map(|(peer, entry)| (*peer, entry.clone()))
            .collect();
        list.sort_by_key(|(_, entry)| entry.since);
        list
    }

    /// 锁中毒（某个线程 panic 了）不该让整个房主端挂掉：拿回数据继续用。
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Gate for SessionList {
    fn admit(&self, hello: &Hello, peer: &Peer) -> Reply {
        let inner = self.lock();
        if inner.closing {
            return Reply::False {
                reason: RejectReason::Closing,
            };
        }
        if inner.kicked.contains(peer) {
            return Reply::False {
                reason: RejectReason::Kicked,
            };
        }
        if hello.proto != crate::control::PROTO_VERSION {
            return Reply::False {
                reason: RejectReason::Version,
            };
        }
        // 注意：这里不写 live，登记在 joined() 里做——控制帧通过 ≠ 数据流已立
        Reply::True
    }

    fn joined(&self, hello: &Hello, peer: &Peer, via_relay: bool) {
        self.join(*peer, &hello.nickname, via_relay);
    }

    fn left(&self, peer: &Peer) {
        self.leave(peer);
    }

    fn watch(&self, peer: &Peer, closer: Closer) {
        SessionList::watch(self, peer, closer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::PROTO_VERSION;

    fn peer(b: u8) -> Peer {
        [b; 32]
    }

    fn hello(proto: u16) -> Hello {
        Hello {
            proto,
            nickname: "小张".to_string(),
            app: "0.1.0".to_string(),
        }
    }

    #[test]
    fn a_normal_guest_is_admitted() {
        let list = SessionList::new();
        assert_eq!(list.admit(&hello(PROTO_VERSION), &peer(1)), Reply::True);
    }

    #[test]
    fn a_kick_really_hangs_up_on_him() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let list = SessionList::new();
        let closed = Arc::new(AtomicUsize::new(0));
        let hit = Arc::clone(&closed);
        list.watch(&peer(1), Arc::new(move || {
            hit.fetch_add(1, Ordering::SeqCst);
        }));
        list.join(peer(1), "小乐", false);
        assert_eq!(list.len(), 1, "先得在名册里");

        list.kick(&peer(1));

        assert_eq!(closed.load(Ordering::SeqCst), 1, "踢人必须真的把连接断掉");
        assert!(list.is_empty(), "踢完名册里不该还有人");
    }

    #[test]
    fn kicking_one_guest_leaves_the_others_alone() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let list = SessionList::new();
        let closed = Arc::new(AtomicUsize::new(0));
        let hit = Arc::clone(&closed);
        list.watch(&peer(2), Arc::new(move || {
            hit.fetch_add(1, Ordering::SeqCst);
        }));
        list.join(peer(1), "先来的", false);
        list.join(peer(2), "后来的", false);

        list.kick(&peer(1));

        assert_eq!(closed.load(Ordering::SeqCst), 0, "别人不该被牵连");
        assert_eq!(list.len(), 1);
        assert_eq!(list.snapshot()[0].0, peer(2));
    }

    #[test]
    fn a_kicked_peer_cannot_come_back() {
        let list = SessionList::new();
        list.join(peer(1), "小张", false);
        list.kick(&peer(1));

        assert_eq!(list.len(), 0, "踢完就不在名册里了");
        assert_eq!(
            list.admit(&hello(PROTO_VERSION), &peer(1)),
            Reply::False {
                reason: RejectReason::Kicked
            },
            "同房间内再敲应当被拒"
        );
    }

    #[test]
    fn reopening_the_door_forgets_the_kick() {
        let list = SessionList::new();
        list.kick(&peer(1));
        list.reopen();
        assert_eq!(list.admit(&hello(PROTO_VERSION), &peer(1)), Reply::True);
    }

    #[test]
    fn a_closing_door_admits_nobody() {
        let list = SessionList::new();
        list.start_closing();
        assert_eq!(
            list.admit(&hello(PROTO_VERSION), &peer(1)),
            Reply::False {
                reason: RejectReason::Closing
            }
        );
    }

    #[test]
    fn a_version_mismatch_is_named_not_guessed() {
        let list = SessionList::new();
        assert_eq!(
            list.admit(&hello(1), &peer(1)),
            Reply::False {
                reason: RejectReason::Version
            }
        );
    }

    #[test]
    fn the_roster_keeps_arrival_order_and_reports_mode() {
        let list = SessionList::new();
        list.join(peer(1), "先来的", false);
        std::thread::sleep(std::time::Duration::from_millis(2));
        list.join(peer(2), "后到的", true);

        let roster = list.snapshot();
        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].1.nickname, "先来的");
        assert!(!roster[0].1.via_relay);
        assert_eq!(roster[1].1.nickname, "后到的");
        assert!(roster[1].1.via_relay, "绕路的人要标出来");
    }

    #[test]
    fn joining_twice_does_not_duplicate() {
        let list = SessionList::new();
        list.join(peer(1), "小张", false);
        list.join(peer(1), "小张", false);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn leaving_removes_from_the_roster_but_not_the_kick_list() {
        let list = SessionList::new();
        list.join(peer(1), "小张", false);
        list.leave(&peer(1));
        assert!(list.is_empty());
        // 正常离开不等于踢过，还能再敲
        assert_eq!(list.admit(&hello(PROTO_VERSION), &peer(1)), Reply::True);
    }

    #[test]
    fn elapsed_time_starts_at_zero() {
        let list = SessionList::new();
        list.join(peer(1), "小张", false);
        assert!(list.snapshot()[0].1.elapsed_secs() < 5);
    }

    /// 同一个人两条连接：敲门那条 + MC 那条。
    /// MC 断了只是少一条，名册里还得有人；两条都断才算走。
    #[test]
    fn a_guest_with_two_connections_survives_one_closing() {
        let list = SessionList::new();
        let p = peer(9);
        list.join(p, "冷枫小乐", false); // 敲门那条
        list.join(p, "冷枫小乐", false); // MC 那条
        assert_eq!(list.len(), 1);

        list.leave(&p); // MC 断开
        assert_eq!(list.len(), 1, "还有一条挂着，人不能消失");

        list.leave(&p); // 敲门那条也断了
        assert_eq!(list.len(), 0, "全断了就是走了");
    }

    /// 踢人不受计数影响：踢了就是踢了。
    #[test]
    fn kicking_beats_the_refcount() {
        let list = SessionList::new();
        let p = peer(10);
        list.join(p, "冷枫小乐", false);
        list.join(p, "冷枫小乐", false);

        list.kick(&p);
        assert_eq!(list.len(), 0);
        assert_eq!(
            list.admit(&hello(PROTO_VERSION), &p),
            Reply::False {
                reason: RejectReason::Kicked
            }
        );
    }
}
