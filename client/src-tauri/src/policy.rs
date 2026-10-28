//! 连接路径的选择：**一次决定，不中途换轨**。
//!
//! 规则来自产品要求（2026-10-28 拍定）：
//! 能打洞就直连，打不通就中继；一旦落定，整场会话锁死该模式，
//! **不允许"直连断了、中继悄悄接上"**。
//!
//! 能强制而不是"尽量"，是因为直连阶段房客端点根本不带中继
//! （`RelayMode::Disabled`）——路径断了没有人能接班。要换轨只能重新绑定，
//! 那是一次新的决定，不是旧连接的续命。

/// 一次拨号尝试的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptKind {
    /// 只打洞，端点上没有任何中继
    Direct,
    /// 只走中继，地址里只有身份 + 中继
    Relay,
}

/// 一次拨号尝试：怎么拨、给多长时间。时间含首字节，不只是握手。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attempt {
    pub kind: AttemptKind,
    pub budget_secs: u64,
}

/// 直连的预算。打洞要么几秒内成，要么就是不成——拖久了只是让用户干等。
pub const DIRECT_BUDGET_SECS: u64 = 5;

/// 中继的预算。多一跳，但路是通的，给宽一点。
pub const RELAY_BUDGET_SECS: u64 = 8;

/// 按房主给的可达性排出尝试顺序。
///
/// 返回空 = 一条路都没有，应当报"屋里没人"，而不是在这里死等。
pub fn plan(has_direct: bool, has_relay: bool) -> Vec<Attempt> {
    let mut attempts = Vec::new();
    if has_direct {
        attempts.push(Attempt {
            kind: AttemptKind::Direct,
            budget_secs: DIRECT_BUDGET_SECS,
        });
    }
    if has_relay {
        attempts.push(Attempt {
            kind: AttemptKind::Relay,
            budget_secs: RELAY_BUDGET_SECS,
        });
    }
    attempts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_first_then_relay() {
        let p = plan(true, true);
        assert_eq!(
            p.iter().map(|a| a.kind).collect::<Vec<_>>(),
            vec![AttemptKind::Direct, AttemptKind::Relay],
            "能打洞就先打洞，打不通才绕路"
        );
        assert!(p[0].budget_secs < p[1].budget_secs, "直连不该比中继等得久");
    }

    #[test]
    fn direct_only_when_there_is_no_relay() {
        let p = plan(true, false);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, AttemptKind::Direct);
    }

    #[test]
    fn relay_only_when_there_is_no_direct_address() {
        // 房主只挂中继（他自己也打不通）：没有直连可试，别浪费时间
        let p = plan(false, true);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].kind, AttemptKind::Relay);
    }

    #[test]
    fn nothing_reachable_means_no_attempt() {
        assert!(plan(false, false).is_empty(), "一条路都没有就别拨号");
    }

    #[test]
    fn a_session_never_switches_tracks() {
        // 策略只产出"尝试序列"，不产出"运行中换轨"这种东西：
        // 序列消费一次就结束，不存在续命入口。
        let p = plan(true, true);
        assert!(p.len() <= 2, "最多两次尝试：直连一次、中继一次");
    }
}
