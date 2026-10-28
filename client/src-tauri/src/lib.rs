//! Knock 隧道核心。
//!
//! 这个 crate 现在只有骨架，目的单一：**证明状态机与接口立得住**。
//! 不包含界面、真实打洞、真实中继（见 AGENTS.md 的第 3 步范围）。
//!
//! 分层：
//! - [`state`]：`ConnState`（7 态）与 `FailReason`（5 码），全项目唯一定义处。
//! - [`machine`]：纯状态机，事件进、状态出。不碰 IO。
//! - [`transport`]：`Transport` trait，界面只向它订阅。
//! - [`fake`]：按脚本驱动的假 transport，用于测试与将来的联调。

pub mod control;
pub mod fake;
pub mod invite;
pub mod iroh_transport;
pub mod machine;
pub mod pipe;
pub mod policy;
pub mod resolve;
pub mod room;
pub mod sessions;
pub mod state;
pub mod transport;

pub use fake::FakeTransport;
pub use invite::{CodeError, Invite, NodeCode};
pub use machine::{Event, IllegalEvent, Machine, RelayPolicy, Role, Update};
pub use state::{ConnState, FailReason};
pub use transport::{StateSink, Target, Transport, TransportError};
