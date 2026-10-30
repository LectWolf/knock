//! 节点列表：去哪儿找别的节点。
//!
//! 规矩（`nodes/README.md`）：**不内置列表**。这里只做三件事——
//! 从仓库里的 `nodes/public.json` 拉一份、存本地、过了保质期再拉。
//! 拉不到就用缓存；连缓存都没有，就让用户手填节点地址。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::room::{Proto, SUPPORTED_API, SUPPORTED_TRANSPORT};

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

/// 所有可用源，按"通常最快/最不容易被墙"排。刷新时挨个试，谁先回谁赢。
///
/// 国内网络下 api.github.com 和 raw.githubusercontent.com 都经常连不上，
/// 所以带上 jsDelivr（CDN）和 ghproxy 这类镜像——这不是"多此一举"，
/// 是没有它们时刷新会直接失败。
pub const SOURCES: &[(&str, &str)] = &[
    ("GitHub API", LIST_URL),
    ("jsDelivr", "https://cdn.jsdelivr.net/gh/LectWolf/knock@master/nodes/public.json"),
    ("ghproxy", "https://ghproxy.net/https://raw.githubusercontent.com/LectWolf/knock/master/nodes/public.json"),
    ("raw", MIRROR_URL),
];

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
/// 挨个源试一遍，用最先回来的那份。
pub fn fetch_any() -> Result<List, String> {
    fetch_with_source().map(|(list, _)| list)
}

/// 同上，但把"哪条源给的"也带出来，界面上可以显示（出问题时能一眼看出是镜像给的旧表）。
pub fn fetch_with_source() -> Result<(List, String), String> {
    let mut errors = Vec::new();
    for (name, url) in SOURCES {
        match fetch(url) {
            Ok(list) => return Ok((list, (*name).to_string())),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    Err(format!("所有源都不行——{}", errors.join("；")))
}

/// 一个节点的体检结果：通不通、多快、支不支持中继。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub url: String,
    /// 往返毫秒；None = 没通
    pub ms: Option<u64>,
    /// 这个节点跑没跑中继
    pub relay: bool,
    /// 节点自报的名字
    pub name: String,
    /// 协议对不对得上（对不上等于用不了，界面要说清楚）
    pub proto_ok: bool,
    /// 节点自报的编码。有它才能拼出完整门牌号（`<节点码>.<房间码>`），
    /// 哪怕这个节点不在任何列表里。
    pub code: Option<String>,
    /// 不通时的原因
    pub error: String,
}

/// 摸一下某个节点：打 /healthz，量个往返。
pub fn probe(url: &str) -> Probe {
    let mut out = Probe {
        url: url.to_string(),
        ms: None,
        relay: false,
        name: String::new(),
        proto_ok: false,
        code: None,
        error: String::new(),
    };
    let started = std::time::Instant::now();
    let endpoint = format!("{}/healthz", url.trim_end_matches('/'));
    let resp = attohttpc::get(&endpoint)
        .timeout(std::time::Duration::from_secs(3))
        .send();
    match resp {
        Ok(r) if r.status().is_success() => {
            out.ms = Some(started.elapsed().as_millis() as u64);
            match r.text() {
                Ok(body) => {
                    #[derive(Deserialize)]
                    struct Health {
                        #[serde(default)]
                        name: String,
                        #[serde(default)]
                        relay_url: String,
                        #[serde(default)]
                        relay_allowed: bool,
                        #[serde(default)]
                        code: String,
                        #[serde(default)]
                        proto: Option<Proto>,
                    }
                    match serde_json::from_str::<Health>(&body) {
                        Ok(h) => {
                            out.name = h.name;
                            let code = h.code.trim().to_ascii_uppercase();
                            if !code.is_empty() {
                                out.code = Some(code);
                            }
                            out.relay = h.relay_allowed && !h.relay_url.is_empty();
                            out.proto_ok = h
                                .proto
                                .map(|p| p.api == SUPPORTED_API && p.transport == SUPPORTED_TRANSPORT)
                                .unwrap_or(false);
                            if !out.proto_ok {
                                out.error = "版本对不上".into();
                            }
                        }
                        Err(e) => out.error = format!("返回看不懂：{e}"),
                    }
                }
                Err(e) => out.error = format!("读不了响应：{e}"),
            }
        }
        Ok(r) => out.error = format!("HTTP {}", r.status()),
        Err(e) => out.error = format!("连不上：{e}"),
    }
    out
}

/// 当前节点的编码从哪儿来：设置里 → 列表缓存 → 问节点自己。
///
/// 门牌号必须是完整邀请（`<节点码>.<房间码>`）。老配置里没存节点码、
/// 或者节点是手工敲地址加进来的，都得靠这个函数补齐。
pub fn code_for(dir: &Path, url: &str, known: &str) -> Option<String> {
    let known = known.trim();
    if NodeCode::decode(known).is_ok() {
        return Some(known.to_ascii_uppercase());
    }
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if let Some(list) = cached(dir) {
        if let Some(e) = list.nodes.iter().find(|e| e.url == url) {
            if !e.code.is_empty() {
                return Some(e.code.clone());
            }
        }
    }
    probe(url).code
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

    #[test]
    fn the_known_code_wins_and_is_normalised() {
        let dir = std::env::temp_dir().join("knock-code-for-1");
        let _ = std::fs::create_dir_all(&dir);
        let code = code_for(&dir, "http://example.com:1", "377pn6h4mph0wyefd838q42c674tr");
        assert_eq!(code.as_deref(), Some("377PN6H4MPH0WYEFD838Q42C674TR"));
    }

    #[test]
    fn an_empty_setting_means_no_code_rather_than_a_wrong_one() {
        let dir = std::env::temp_dir().join("knock-code-for-2");
        let _ = std::fs::create_dir_all(&dir);
        assert!(code_for(&dir, "", "").is_none());
    }

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

#[cfg(test)]
mod probe_tests {
    use super::*;

    /// 摸真节点：这条要能说出延迟、中继、协议三件事。
    #[test]
    #[ignore]
    fn the_real_node_answers_the_probe() {
        let url = std::env::var("KNOCK_NODE_URL")
            .unwrap_or_else(|_| "http://154.36.165.162:30990".to_string());
        let p = probe(&url);
        println!("{p:?}");
        assert!(p.ms.is_some(), "节点应该通：{:?}", p.error);
        assert!(p.proto_ok, "协议应当对得上");
        // 中继是节点自己的选择：香港那台带，枣庄那台没带，两条都算正常
        println!("中继：{}", if p.relay { "有" } else { "无" });
        let ms = p.ms.unwrap();
        assert!(p.code.is_some(), "healthz 里没报节点码：客户端拼不出完整门牌号");
        assert!(ms < 2000, "延迟离谱：{ms} ms");
    }

    /// 不通的地址要老实说"连不上"，而不是装成功。
    #[test]
    #[ignore]
    fn a_dead_address_is_reported_as_dead() {
        let p = probe("http://127.0.0.1:1");
        assert!(p.ms.is_none());
        assert!(!p.error.is_empty());
    }
}
