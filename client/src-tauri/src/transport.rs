//! `Transport`：界面唯一可以依赖的连接接口。
//!
//! 硬规矩（AGENTS.md 第 1 条）：**状态迁移只发生在 transport 内部**。
//! 界面调 [`Transport::connect`]，然后只消费 [`Transport::on_state`] 推来的 [`Update`]。
//! 界面里写 `if 打洞中 { 显示转圈 }` 是错误做法。

use crate::machine::{IllegalEvent, Update};
use crate::state::ConnState;

/// 发起连接的意图。房主和房客走的是同一套状态机，但入口不同。
///
/// 这里没有做成 `connect(code: &str)`：房主也要进状态机（`creating`），
/// 用"空字符串表示开门"会把语义藏进魔法值，正是词表在消灭的那种含糊。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target<'a> {
    /// 房主：开门，代理本机的这个端口
    Open { port: u16 },
    /// 房客：敲门，参数是邀请串或门牌号
    Knock { invite: &'a str },
}

impl Target<'_> {
    pub fn port(&self) -> Option<u16> {
        match self {
            Target::Open { port } => Some(*port),
            Target::Knock { .. } => None,
        }
    }
}

/// 状态订阅回调。可以注册多个之外的语义由实现决定，骨架期只留一个。
pub type StateSink = Box<dyn FnMut(&Update) + Send>;

/// transport 层的错误。**注意：连接失败不走这里**——
/// 那是状态机里 `failed` + `FailReason` 的事。这里只报"接口被用错了"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportError {
    /// 还没 connect 就 feed/close
    NotConnected,
    /// 当前状态下不该收到这个事件
    IllegalEvent(IllegalEvent),
}

pub trait Transport {
    /// 开始一次连接。返回 `Ok` 只表示"已受理"，结果一律由状态回调给出。
    fn connect(&mut self, target: Target<'_>) -> Result<(), TransportError>;

    /// 订阅状态。必须在 `connect` 之前调用，否则会漏掉 `idle -> ...` 的头几步。
    fn on_state(&mut self, sink: StateSink);

    /// 关门/挂断。幂等：没连上时调用不报错。
    fn close(&mut self);

    /// 当前状态。给界面做首帧渲染用，**不是**订阅的替代品。
    fn state(&self) -> ConnState;
}
