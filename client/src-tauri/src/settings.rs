//! 本地设置：昵称、节点、服务号、绕路偏好、外观。
//!
//! 只存本机 `%APPDATA%\cc.mcii.knock\settings.json`——不注册、不上传。
//! 房间和名册都是易失数据，**不落盘**（见 ADR-0004）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// 朋友敲门时看到的名字
    #[serde(default)]
    pub nickname: String,
    /// 节点地址，比如 http://154.36.165.162:30990
    #[serde(default)]
    pub node: String,
    /// 节点的显示名（「香港·荃湾」）。节点自己上报的名字优先，这只是给人看的缓存。
    #[serde(default)]
    pub node_name: String,
    /// 选中节点的**编码原文**。
    ///
    /// 门牌号是「节点码 + 房间码」一整串，所以必须记住节点码：
    /// 光有 URL 拼不出那串东西，朋友就得先去节点列表里找节点。
    #[serde(default)]
    pub node_code: String,
    /// 本机 MC 服务在哪个端口
    #[serde(default = "default_port")]
    pub port: u16,
    /// 打不通时怎么办：auto / ask / never
    #[serde(default = "default_relay")]
    pub relay: String,
    /// 外观：auto / light / dark
    #[serde(default = "default_theme")]
    pub theme: String,
}

fn default_port() -> u16 {
    25565
}
fn default_relay() -> String {
    "auto".to_string()
}
fn default_theme() -> String {
    "auto".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            nickname: String::new(),
            node: String::new(),
            node_name: String::new(),
            node_code: String::new(),
            port: default_port(),
            relay: default_relay(),
            theme: default_theme(),
        }
    }
}

impl Settings {
    /// 昵称必须能被 `control::clean_nickname` 接受；这里只做"有没有"的判断。
    pub fn is_ready(&self) -> bool {
        !self.nickname.trim().is_empty() && !self.node.trim().is_empty()
    }
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join("settings.json")
}

/// 读设置。文件不存在/坏了都退到默认值——**设置读不出来不该让应用起不来**。
pub fn load(dir: &Path) -> Settings {
    let p = path(dir);
    let Ok(raw) = std::fs::read_to_string(&p) else {
        return Settings::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save(dir: &Path, settings: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let raw = serde_json::to_string_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path(dir), raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_falls_back_to_defaults() {
        let dir = std::env::temp_dir().join("knock-settings-none");
        let _ = std::fs::remove_dir_all(&dir);
        let s = load(&dir);
        assert_eq!(s.port, 25565);
        assert_eq!(s.relay, "auto");
        assert!(!s.is_ready(), "没昵称没节点时不算配置好");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = std::env::temp_dir().join("knock-settings-round");
        let _ = std::fs::remove_dir_all(&dir);
        let mut s = Settings::default();
        s.nickname = "小张".into();
        s.node = "http://127.0.0.1:30990".into();
        s.port = 25566;
        save(&dir, &s).unwrap();

        let back = load(&dir);
        assert_eq!(back.nickname, "小张");
        assert_eq!(back.port, 25566);
        assert!(back.is_ready());
    }

    #[test]
    fn a_corrupt_file_does_not_panic() {
        let dir = std::env::temp_dir().join("knock-settings-bad");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path(&dir), b"{not json").unwrap();
        assert_eq!(load(&dir).port, 25565);
    }
}
