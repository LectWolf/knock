//! 按脚本驱动的假 transport。
//!
//! 它的职责只有一件：**把事件按剧本喂给真正的状态机**，然后把状态机吐出的
//! [`Update`] 通过 `on_state` 推给订阅者。
//!
//! 它自己不做任何状态判断——所以测试断言的是状态机的行为，而不是假 transport 的复读。
//! 将来真实的 transport（iroh）只要换掉"事件从哪来"，状态机与测试都不用改。

use crate::machine::{Event, Machine, RelayPolicy, Role, Update};
use crate::state::ConnState;
use crate::transport::{StateSink, Target, Transport, TransportError};

pub struct FakeTransport {
    /// 入口事件之后的剧本
    script: Vec<Event>,
    policy: RelayPolicy,
    sink: Option<StateSink>,
    machine: Option<Machine>,
    updates: Vec<Update>,
}

impl FakeTransport {
    /// 剧本 = 入口事件之后依次发生的事。入口事件（开门/敲门）由 `connect` 自己发。
    pub fn new(script: Vec<Event>) -> Self {
        Self { script, policy: RelayPolicy::Auto, sink: None, machine: None, updates: Vec::new() }
    }

    pub fn with_policy(mut self, policy: RelayPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// 收到过的全部状态快照，按时间顺序。
    pub fn updates(&self) -> &[Update] {
        &self.updates
    }

    /// 只要状态序列，测试里读起来更清楚。
    pub fn states(&self) -> Vec<ConnState> {
        self.updates.iter().map(|u| u.state).collect()
    }

    pub fn fail(&self) -> Option<crate::state::FailReason> {
        self.updates.last().and_then(|u| u.fail)
    }

    /// 手动补一个事件。用于 `RelayPolicy::Ask` 这类"要等用户回答"的场景。
    pub fn feed(&mut self, event: Event) -> Result<(), TransportError> {
        let machine = self.machine.as_mut().ok_or(TransportError::NotConnected)?;
        let update = machine.apply(event).map_err(TransportError::IllegalEvent)?;
        self.record(update);
        Ok(())
    }

    fn record(&mut self, update: Update) {
        self.updates.push(update);
        if let Some(sink) = self.sink.as_mut() {
            sink(&update);
        }
    }
}

impl Transport for FakeTransport {
    fn connect(&mut self, target: Target<'_>) -> Result<(), TransportError> {
        let (role, entry) = match target {
            Target::Open { .. } => (Role::Host, Event::OpenRequested),
            Target::Knock { .. } => (Role::Guest, Event::KnockSubmitted),
        };

        self.machine = Some(Machine::new(role, self.policy));
        self.updates.clear();

        // 首帧快照：界面第一帧就能渲染，不必等第一次迁移。
        self.record(Update::new(ConnState::Idle, None, false));
        self.feed(entry)?;

        let script = std::mem::take(&mut self.script);
        for event in script {
            self.feed(event)?;
        }
        Ok(())
    }

    fn on_state(&mut self, sink: StateSink) {
        self.sink = Some(sink);
    }

    fn close(&mut self) {
        if self.machine.is_some() {
            // 关门失败没有意义（已经断了），忽略错误。
            let _ = self.feed(Event::CloseRequested);
            self.machine = None;
        }
    }

    fn state(&self) -> ConnState {
        self.machine.as_ref().map(|m| m.state()).unwrap_or(ConnState::Idle)
    }
}
