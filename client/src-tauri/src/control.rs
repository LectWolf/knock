//! 控制通道 `knock/2`：房客一上来先自报家门，房主再决定放不放。
//!
//! 没有这一层，房主就是瞎的：不知道谁进来了，也没法把人赶出去。
//! 有了它，"看到谁""踢谁""他叫啥""版本对不对"全都有地方附着。
//!
//! 帧格式刻意土：`[2 字节大端长度][JSON]`。
//! 不做通用序列化框架，因为控制帧一辈子就两句半话，可读性比扩展性重要。

use serde::{Deserialize, Serialize};

/// 控制帧上限。控制帧不该长，写死上限是防滥用的第一道门。
pub const MAX_FRAME: usize = 512;

/// 协议版本。房主和房客都报这个，对不上就明说。
pub const PROTO_VERSION: u16 = 2;

/// 昵称长度：太短记不住，太长列表里放不下。
pub const NICKNAME_MAX_CHARS: usize = 24;

/// 房客的自报家门。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// 协议版本
    pub proto: u16,
    /// 昵称（用户首次启动时设的）
    pub nickname: String,
    /// 客户端版本，只用于排查，不参与判定
    #[serde(default)]
    pub app: String,
}

/// 房主的回答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ok", rename_all = "snake_case")]
pub enum Reply {
    /// 放进来，后面才开始传数据
    True,
    /// 不放，给个能被翻译成人话的原因
    False { reason: RejectReason },
}

/// 拒绝原因。每一个都能对应到界面上的一句话。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// 被房主踢过（本房间内记住）
    Kicked,
    /// 协议版本对不上 —— 多半是一边没更新
    Version,
    /// 房主正在关这扇门
    Closing,
    /// 门开着，但这家的游戏服务没起来
    HostOffline,
}

/// 帧编解码的失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// 超过上限
    TooLong,
    /// 长度前缀说的和实际收到的对不上
    Truncated,
    /// JSON 不合法
    Malformed(String),
    /// 内容合法但没用（比如昵称是空的）
    Invalid(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLong => write!(f, "控制帧太长了"),
            FrameError::Truncated => write!(f, "控制帧被截断了"),
            FrameError::Malformed(e) => write!(f, "控制帧格式不对：{e}"),
            FrameError::Invalid(e) => write!(f, "控制帧内容不合法：{e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// 把一帧编成字节（含 2 字节长度前缀）。
pub fn encode<T: Serialize>(frame: &T) -> Result<Vec<u8>, FrameError> {
    let body = serde_json::to_vec(frame).map_err(|e| FrameError::Malformed(e.to_string()))?;
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLong);
    }
    let mut out = Vec::with_capacity(body.len() + 2);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// 从字节里解一帧。要求长度前缀和实际长度严格一致。
pub fn decode<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Result<T, FrameError> {
    if buf.len() < 2 {
        return Err(FrameError::Truncated);
    }
    let declared = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if declared > MAX_FRAME {
        return Err(FrameError::TooLong);
    }
    let body = &buf[2..];
    if body.len() != declared {
        return Err(FrameError::Truncated);
    }
    serde_json::from_slice(body).map_err(|e| FrameError::Malformed(e.to_string()))
}

/// 校验昵称：去空白后不能为空，不能太长，不能有控制字符。
pub fn clean_nickname(raw: &str) -> Result<String, FrameError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(FrameError::Invalid("昵称不能是空的".to_string()));
    }
    if trimmed.chars().count() > NICKNAME_MAX_CHARS {
        return Err(FrameError::Invalid(format!(
            "昵称最多 {NICKNAME_MAX_CHARS} 个字"
        )));
    }
    if trimmed.chars().any(|c| c.is_control()) {
        return Err(FrameError::Invalid("昵称里不能有控制字符".to_string()));
    }
    Ok(trimmed.to_string())
}

/// 房主决定放不放人时看的全部依据。
pub struct Verdict<'a> {
    /// 房客报的版本
    pub hello: &'a Hello,
    /// 是否被踢过（本房间内）
    pub kicked: bool,
    /// 门是不是正在关
    pub closing: bool,
}

/// 判定：先看门，再看人，最后看版本。
///
/// 顺序有讲究：门都关了就不必再解释版本问题。
pub fn judge(v: &Verdict<'_>) -> Reply {
    if v.closing {
        return Reply::False {
            reason: RejectReason::Closing,
        };
    }
    if v.kicked {
        return Reply::False {
            reason: RejectReason::Kicked,
        };
    }
    if v.hello.proto != PROTO_VERSION {
        return Reply::False {
            reason: RejectReason::Version,
        };
    }
    Reply::True
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello(nickname: &str) -> Hello {
        Hello {
            proto: PROTO_VERSION,
            nickname: nickname.to_string(),
            app: "0.1.0".to_string(),
        }
    }

    #[test]
    fn a_hello_round_trips() {
        let frame = hello("小张");
        let bytes = encode(&frame).expect("编得出来");
        assert!(bytes.len() <= MAX_FRAME + 2);
        let back: Hello = decode(&bytes).expect("解得回去");
        assert_eq!(back, frame);
    }

    #[test]
    fn an_accept_reply_says_ok_true() {
        let bytes = encode(&Reply::True).unwrap();
        let json = std::str::from_utf8(&bytes[2..]).unwrap();
        assert!(json.contains("\"ok\":\"true\""), "实际：{json}");
        let back: Reply = decode(&bytes).unwrap();
        assert_eq!(back, Reply::True);
    }

    #[test]
    fn a_kick_reply_carries_the_reason() {
        let bytes = encode(&Reply::False {
            reason: RejectReason::Kicked,
        })
        .unwrap();
        let back: Reply = decode(&bytes).unwrap();
        assert_eq!(
            back,
            Reply::False {
                reason: RejectReason::Kicked
            }
        );
    }

    #[test]
    fn oversize_frames_are_refused() {
        let fat = hello(&"长".repeat(MAX_FRAME));
        assert_eq!(encode(&fat), Err(FrameError::TooLong));
    }

    #[test]
    fn truncated_frames_are_refused() {
        let bytes = encode(&hello("小张")).unwrap();
        // 截掉最后一个字节：长度前缀还在，内容不完整
        assert_eq!(decode::<Hello>(&bytes[..bytes.len() - 1]), Err(FrameError::Truncated));
        // 只有长度前缀的一半
        assert_eq!(decode::<Hello>(&[0u8]), Err(FrameError::Truncated));
    }

    #[test]
    fn garbage_payloads_are_refused() {
        let mut bytes = vec![0, 3];
        bytes.extend_from_slice(b"{{{");
        assert!(matches!(decode::<Hello>(&bytes), Err(FrameError::Malformed(_))));
    }

    #[test]
    fn nicknames_are_trimmed_and_bounded() {
        assert_eq!(clean_nickname("  小张  ").unwrap(), "小张");
        assert!(clean_nickname("").is_err());
        assert!(clean_nickname("   ").is_err());
        assert!(clean_nickname(&"长".repeat(NICKNAME_MAX_CHARS + 1)).is_err());
        assert!(clean_nickname("坏\u{0}名字").is_err());
        // 正好到上限是允许的
        assert!(clean_nickname(&"长".repeat(NICKNAME_MAX_CHARS)).is_ok());
    }

    #[test]
    fn the_door_being_closed_wins_over_everything() {
        let h = hello("小张");
        let reply = judge(&Verdict {
            hello: &h,
            kicked: true,
            closing: true,
        });
        assert_eq!(
            reply,
            Reply::False {
                reason: RejectReason::Closing
            }
        );
    }

    #[test]
    fn a_kicked_peer_is_refused_even_with_the_right_version() {
        let h = hello("小张");
        let reply = judge(&Verdict {
            hello: &h,
            kicked: true,
            closing: false,
        });
        assert_eq!(
            reply,
            Reply::False {
                reason: RejectReason::Kicked
            }
        );
    }

    #[test]
    fn a_version_mismatch_is_named_not_guessed() {
        let mut h = hello("小张");
        h.proto = 1;
        let reply = judge(&Verdict {
            hello: &h,
            kicked: false,
            closing: false,
        });
        assert_eq!(
            reply,
            Reply::False {
                reason: RejectReason::Version
            }
        );
    }

    #[test]
    fn a_normal_guest_gets_in() {
        let h = hello("小张");
        assert_eq!(
            judge(&Verdict {
                hello: &h,
                kicked: false,
                closing: false
            }),
            Reply::True
        );
    }
}
