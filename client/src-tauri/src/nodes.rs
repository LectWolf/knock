//! 节点列表：去哪儿找别的节点。
//!
//! 规矩（`nodes/README.md`）：**不内置列表**。这里只做三件事——
//! 从仓库里的 `nodes/public.json` 拉一份、存本地、过了保质期再拉。
//! 拉不到就用缓存；连缓存都没有，就让用户手填节点地址。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::invite::NodeCode;

/// 列表放哪。改这里等于改"大家从哪儿认识新节点"，是有公关后果的，别乱改。
///
/// 用 API 而不用 raw.githubusercontent：后者在国内网络里经常连不上，
/// 而 api.github.com 一般能通。`Accept: application/vnd.github.raw` 拿到的就是原文。
pub const LIST_URL: &str =
    "https://api.github.com/repos/LectWolf/knock/contents/nodes/public.json?ref=master";

/// 备选：API 挂了的时候再试这条（能通说明网络环境不需要 API）。
pub const MIRROR_URL: &str =
    "https://raw.githubusercontent.com/LectWolf/knock/master/nodes/public.json";

/// 缓存多久算馊了。节点会下线，列表太旧等于骗人。
pub const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 3600);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub code: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub relay: bool,
    #[serde(default)]
    pub note: String,
    /// 从 `code` 里解出来的 HTTP 地址。列表本身不写这个字段，是解析时补的。
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct List {
    /// 什么时候拉的（unix 秒）
    #[serde(default)]
    pub fetched_at: u64,
    pub nodes: Vec<Entry>,
}

#[derive(Deserialize)]
struct Raw {
    nodes: Vec<Entry>,
}

impl List {
    pub fn is_stale(&self, now: u64) -> bool {
        now.saturating_sub(self.fetched_at) > MAX_AGE.as_secs()
    }

    /// 这串节点码对应的 HTTP 地址。解析不出来就 `None`——列表里有脏数据不该让界面崩。
    pub fn url_of(code: &str) -> Option<String> {
        let node = NodeCode::decode(code).ok()?;
        let host = match (&node.domain, node.ipv4) {
            (Some(domain), _) if !domain.is_empty() => domain.clone(),
            (_, Some(ip)) => format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
            _ => return None,
        };
        Some(format!("http://{host}:{}", node.port))
    }
}

pub fn path(dir: &Path) -> PathBuf {
    dir.join("nodes.json")
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 读缓存。坏了/没有就 `None`，不当错误处理——列表只是方便，不是必需品。
pub fn cached(dir: &Path) -> Option<List> {
    let raw = std::fs::read_to_string(path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save(dir: &Path, list: &List) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let raw = serde_json::to_string_pretty(list)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path(dir), raw)
}

/// 去拉一份新的。失败返回人话，界面直接显示。
pub fn fetch(url: &str) -> Result<List, String> {
    let response = attohttpc::get(url)
        // GitHub API 要求带 UA；raw 那条不吃这个头，带了也无害
        .header("User-Agent", "knock-client")
        .header("Accept", "application/vnd.github.raw")
        .timeout(Duration::from_secs(8))
        .send()
        .map_err(|e| format!("连不上列表：{e}"))?;
    if !response.status().is_success() {
        return Err(format!("列表取不回来：HTTP {}", response.status().as_u16()));
    }
    let text = response.text().map_err(|e| format!("读不出列表：{e}"))?;
    parse(&text)
}

/// 先走 API，不通再走 raw。两条都不通才报错，错误里说清楚两条都试过了。
pub fn fetch_any() -> Result<List, String> {
    match fetch(LIST_URL) {
        Ok(list) => Ok(list),
        Err(first) => fetch(MIRROR_URL).map_err(|second| format!("{first}；备用地址也不行：{second}")),
    }
}

pub fn parse(text: &str) -> Result<List, String> {
    let mut raw: Raw = serde_json::from_str(text).map_err(|e| format!("列表格式不对：{e}"))?;
    for entry in &mut raw.nodes {
        entry.url = List::url_of(&entry.code).unwrap_or_default();
    }
    Ok(List {
        fetched_at: now_secs(),
        nodes: raw.nodes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "version": 1,
      "nodes": [
        {"code": "377PN6H4MPH0WYEFD838Q42C674TR", "label": "0TPF",
         "name": "香港·荃湾", "relay": true, "note": "测试节点"}
      ]
    }"#;

    #[test]
    fn parses_the_repo_list() {
        let list = parse(SAMPLE).unwrap();
        assert_eq!(list.nodes.len(), 1);
        assert_eq!(list.nodes[0].name, "香港·荃湾");
        assert!(list.nodes[0].url.starts_with("http://"), "解析时要把码翻成地址");
        assert!(list.nodes[0].relay);
        assert!(list.fetched_at > 0);
    }

    #[test]
    fn a_code_becomes_a_url() {
        // 列表里存的是节点码，客户端要的是 HTTP 地址
        let url = List::url_of("377PN6H4MPH0WYEFD838Q42C674TR").expect("能从码里取出地址");
        assert!(url.starts_with("http://"), "{url}");
        assert!(url.contains(":30990"), "{url}");
    }

    #[test]
    fn garbage_codes_do_not_panic() {
        assert_eq!(List::url_of("nonsense"), None);
        assert_eq!(List::url_of(""), None);
    }

    #[test]
    fn staleness_is_measured_in_days() {
        let fresh = List {
            fetched_at: 1_000,
            nodes: vec![],
        };
        assert!(!fresh.is_stale(1_000 + 3600));
        assert!(fresh.is_stale(1_000 + MAX_AGE.as_secs() + 1));
    }

    /// 真联网：只在手动跑的时候验证，CI 不该依赖外网。
    #[test]
    #[ignore]
    fn the_published_list_is_fetchable() {
        let list = fetch_any().expect("GitHub 上的列表应该拉得到");
        assert!(!list.nodes.is_empty(), "列表不该是空的");
        assert!(list.nodes[0].url.starts_with("http://"));
    }

    #[test]
    fn cache_round_trips_on_disk() {
        let dir = std::env::temp_dir().join("knock-nodes-cache");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(cached(&dir).is_none());

        let list = parse(SAMPLE).unwrap();
        save(&dir, &list).unwrap();
        let back = cached(&dir).expect("存了就该读得到");
        assert_eq!(back.nodes.len(), 1);
        assert_eq!(back.nodes[0].label, "0TPF");
    }
}
