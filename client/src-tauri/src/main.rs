// 发布版不要跟一个黑框框：控制台窗口只有在调试时才该出现。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! 桌面壳：主窗口 + 托盘 + 界面命令。
//!
//! 分工：
//!   - 网络与状态机全在 `knock` 库里，这里**不判断连接逻辑**
//!   - 这个文件只做三件事：装配 transport、把 `Update` 推给界面、响应托盘

mod logging;

use std::sync::{Arc, Mutex};

use knock::iroh_transport::IrohTransport;
use knock::machine::Update;
use knock::room::HttpRoomClient;
use knock::settings::{self, Settings};
use knock::state::{ConnState, FailReason};
use knock::transport::{Target, Transport};
use serde::Serialize;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State, WindowEvent,
};

/// 应用共享状态。transport 为 `None` 表示现在没开门也没敲门。
#[derive(Default)]
struct App {
    transport: Mutex<Option<IrohTransport>>,
}

/// 上一次记过的门牌号：界面会轮询，只在变化时写日志。
static LAST_DOOR_CODE: Mutex<String> = Mutex::new(String::new());
/// 上一次记过的本机入口地址（房客那边要看它去游戏里连）。
static LAST_ENTRY: Mutex<String> = Mutex::new(String::new());

/// 界面右下角显示的版本号。以编译时的 Cargo 版本为准，别在界面上手写。
#[tauri::command]
fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 日志落在哪儿——给界面的「看日志」用。
#[tauri::command]
fn log_path() -> String {
    logging::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（日志还没起来）".to_string())
}

#[tauri::command]
fn open_log() -> Result<(), String> {
    klog!("INFO", "用户点了「看日志」");
    logging::reveal()
}

/// 打日志用的设置摘要：别把整份设置都写上去，够定位就行。
fn settings_summary(s: &Settings) -> String {
    format!(
        "node={:?} name={:?} code={} port={} relay={} theme={} nick={:?}",
        s.node,
        s.node_name,
        if s.node_code.is_empty() { "（空）" } else { &s.node_code },
        s.port,
        s.relay,
        s.theme,
        s.nickname
    )
}

fn config_dir(app: &AppHandle) -> std::path::PathBuf {
    app.path()
        .app_config_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
}

fn state_name(s: ConnState) -> &'static str {
    match s {
        ConnState::Idle => "idle",
        ConnState::Creating => "creating",
        ConnState::Waiting => "waiting",
        ConnState::Punching => "punching",
        ConnState::Direct => "direct",
        ConnState::Relay => "relay",
        ConnState::Failed => "failed",
    }
}

fn fail_name(f: FailReason) -> &'static str {
    match f {
        FailReason::CodeNotFound => "code_not_found",
        FailReason::HostOffline => "host_offline",
        FailReason::BlockedByFirewall => "blocked_by_firewall",
        FailReason::SymmetricNat => "symmetric_nat",
        FailReason::RelayUnavailable => "relay_unavailable",
        FailReason::NodeOutdated => "node_outdated",
    }
}

/// 推给界面的状态。字段名和前端那套一一对应。
#[derive(Serialize, Clone)]
struct StatePayload {
    state: &'static str,
    fail: Option<&'static str>,
    relay_offer: bool,
}

#[derive(Serialize)]
struct GuestView {
    id: String,
    nickname: String,
    via_relay: bool,
    secs: u64,
}

#[tauri::command]
fn load_settings(app: AppHandle) -> Settings {
    let s = settings::load(&config_dir(&app));
    klog!("INFO", "读设置：{}", settings_summary(&s));
    s
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    klog!("INFO", "存设置：{}", settings_summary(&settings));
    settings::save(&config_dir(&app), &settings).map_err(|e| {
        klog!("ERROR", "存设置失败：{e}");
        e.to_string()
    })
}

/// 装配一次 transport：同一个节点、同一套状态回调。
fn build_transport(app: &AppHandle, s: &Settings) -> Result<IrohTransport, String> {
    if s.node.trim().is_empty() {
        klog!("WARN", "没配节点，开不了门也敲不了门");
        return Err("先添加一个节点".into());
    }
    klog!("INFO", "装配 transport：节点 {}", s.node);
    let client = Arc::new(HttpRoomClient::new(s.node.clone()));
    let mut transport = IrohTransport::new(client).with_nickname(s.nickname.clone());

    // 状态只在 transport 内部迁移；这里只负责搬给界面
    let handle = app.clone();
    transport.on_state(Box::new(move |u: &Update| {
        klog!(
            "INFO",
            "状态 -> {}（原因 {:?}，可选绕路 {})",
            state_name(u.state),
            u.fail.map(fail_name),
            u.relay_offer
        );
        let payload = StatePayload {
            state: state_name(u.state),
            fail: u.fail.map(fail_name),
            relay_offer: u.relay_offer,
        };
        let _ = handle.emit("knock://state", payload);
    }));
    klog!("INFO", "transport 就绪");
    Ok(transport)
}

#[tauri::command]
fn open_door(app: AppHandle, st: State<'_, App>, port: u16) -> Result<(), String> {
    let mut s = settings::load(&config_dir(&app));
    // 门牌号要带节点码，缺了就现在补（列表缓存 → 问节点自己），补到就存下来。
    klog!("INFO", "开门请求：端口 {}，当前设置 {}", port, settings_summary(&s));
    if let Some(code) = knock::nodes::code_for(&config_dir(&app), &s.node, &s.node_code) {
        if s.node_code != code {
            klog!("INFO", "补上节点码：{:?} -> {:?}", s.node_code, code);
            s.node_code = code;
            let _ = settings::save(&config_dir(&app), &s);
        }
    } else {
        klog!("WARN", "拿不到节点码：门牌号会退化成短码（只能显示 6 位房间码）");
    }
    let mut transport = build_transport(&app, &s)?;
    transport.connect(Target::Open { port }).map_err(|e| {
        klog!("ERROR", "开门失败：{e:?}");
        format!("{e:?}")
    })?;
    *st.transport.lock().unwrap() = Some(transport);
    klog!("INFO", "门已经在开（等节点分配房间）");
    Ok(())
}

#[tauri::command]
fn knock(app: AppHandle, st: State<'_, App>, invite: String) -> Result<(), String> {
    let mut s = settings::load(&config_dir(&app));
    klog!("INFO", "敲门请求：{invite:?}，当前设置 {}", settings_summary(&s));

    // 门牌号可能自带节点码（完整邀请）。那就用**那串里写的节点**，
    // 而不是设置里当前选的——否则朋友发来一整串，你还得先去节点列表里加节点。
    if let Ok(knock::invite::Invite::Full { node, .. }) = knock::invite::Invite::parse(&invite) {
        if let Some(text) = node.encode().ok() {
            if let Some(url) = knock::nodes::List::url_of(&text) {
                if url != s.node {
                    s.node = url;
                    s.node_name = String::new();
                }
            }
            if s.node_code != text {
                s.node_code = text;
            }
            let _ = settings::save(&config_dir(&app), &s);
        }
    }
    let mut transport = build_transport(&app, &s)?;
    transport.connect(Target::Knock { invite: &invite }).map_err(|e| {
        klog!("ERROR", "敲门受理失败：{e:?}");
        format!("{e:?}")
    })?;
    *st.transport.lock().unwrap() = Some(transport);
    Ok(())
}

#[tauri::command]
fn close_door(st: State<'_, App>) {
    if let Some(t) = st.transport.lock().unwrap().as_mut() {
        klog!("INFO", "关门（当前状态 {}）", state_name(t.state()));
        t.close();
    } else {
        klog!("INFO", "关门：本来就没门");
    }
    *st.transport.lock().unwrap() = None;
}

/// 列表：先给缓存（哪怕过期了，也比空着强），界面自己决定要不要刷新。
#[tauri::command]
fn nodes_cached(app: AppHandle) -> Option<knock::nodes::List> {
    let list = knock::nodes::cached(&config_dir(&app));
    klog!(
        "INFO",
        "节点缓存：{}",
        list.as_ref().map(|l| format!("{} 台，{} 秒前拉的", l.nodes.len(), knock::nodes::now_secs().saturating_sub(l.fetched_at))).unwrap_or_else(|| "没有".into())
    );
    list
}

/// 去 GitHub 重新拉一份。拉不到就把人话原样带回界面。
#[tauri::command]
async fn nodes_refresh(app: AppHandle) -> Result<knock::nodes::List, String> {
    // 拉列表要发网络请求，**绝不能**在主线程上同步做——那会把界面卡死（之前就是这样）
    klog!("INFO", "拉节点列表……");
    let started = std::time::Instant::now();
    let (list, source) = tauri::async_runtime::spawn_blocking(knock::nodes::fetch_with_source)
        .await
        .map_err(|e| {
            klog!("ERROR", "拉列表失败：{e}");
            format!("拉列表失败：{e}")
        })??;
    klog!(
        "INFO",
        "列表到手：{} 台，来自 {}，用了 {} ms",
        list.nodes.len(),
        source,
        started.elapsed().as_millis()
    );
    let dir = config_dir(&app);
    // 存不下不算失败：这次已经能用上了
    let _ = knock::nodes::save(&dir, &list);
    Ok(list)
}

/// 房客这一侧本机该连哪儿。拿到 `direct`/`relay` 之后界面要把它写出来，
/// 不然玩家不知道在游戏里填什么地址——这是"能不能真的玩"的关键一环。
#[tauri::command]
fn entry_addr(st: State<'_, App>) -> Option<String> {
    let addr = st
        .transport
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|t| t.local_addr())
        .map(|a| a.to_string());
    if let Some(a) = &addr {
        if *LAST_ENTRY.lock().unwrap() != *a {
            klog!("INFO", "游戏里连这里：{a}");
            *LAST_ENTRY.lock().unwrap() = a.clone();
        }
    }
    addr
}

/// 逐个摸节点的延迟和"支不支持中继"。同样放到后台线程，界面不卡。
#[tauri::command]
async fn nodes_probe(urls: Vec<String>) -> Vec<knock::nodes::Probe> {
    klog!("INFO", "探节点 {}", urls.len());
    tauri::async_runtime::spawn_blocking(move || {
        let list: Vec<_> = urls
            .into_iter()
            .map(|u| {
                let p = knock::nodes::probe(&u);
                klog!(
                    "INFO",
                    "  节点 {u} -> ms={:?} relay={} proto_ok={} code={:?} err={:?}",
                    p.ms,
                    p.relay,
                    p.proto_ok,
                    p.code,
                    p.error
                );
                p
            })
            .collect();
        list
    })
    .await
    .unwrap_or_default()
}

/// 把一串节点编码解析成能用的地址。自定义节点就靠它。
///
/// 解析不出来返回 `None`——界面据此说"这串码认不出来"，而不是拿它去发请求。
#[tauri::command]
fn node_url_from_code(code: String) -> Option<String> {
    let url = knock::nodes::List::url_of(code.trim());
    klog!("INFO", "解析节点码 {} -> {url:?}", code.trim());
    url
}

/// 现在这扇门的门牌号。
///
/// **门牌号就是完整邀请**：`<节点码>.<房间码>`。一整串发给朋友，他粘贴就能敲，
/// 不用先上节点列表找节点。知道节点码的时候绝不给半串。
#[tauri::command]
fn door_code(app: AppHandle, st: State<'_, App>) -> Option<String> {
    let room = st
        .transport
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|t| t.room_code())?;
    let mut s = settings::load(&config_dir(&app));
    if knock::invite::NodeCode::decode(s.node_code.trim()).is_err() {
        // 不开门也能补：列表缓存里有就先用上，界面不用等网络。
        if let Some(list) = knock::nodes::cached(&config_dir(&app)) {
            if let Some(e) = list.nodes.iter().find(|e| e.url == s.node.trim()) {
                if !e.code.is_empty() {
                    s.node_code = e.code.clone();
                }
            }
        }
    }
    let label = knock::invite::door_label(&s.node_code, &room);
    if label != *LAST_DOOR_CODE.lock().unwrap() {
        if label.contains('.') {
            klog!("INFO", "门牌号：{label}");
        } else {
            klog!("WARN", "门牌号只有短码 {label}（没有节点码，朋友得先加节点）");
        }
        *LAST_DOOR_CODE.lock().unwrap() = label.clone();
    }
    Some(label)
}

#[tauri::command]
fn door_state(st: State<'_, App>) -> String {
    st.transport
        .lock()
        .unwrap()
        .as_ref()
        .map(|t| state_name(t.state()).to_string())
        .unwrap_or_else(|| "idle".to_string())
}

#[tauri::command]
fn guests(st: State<'_, App>) -> Vec<GuestView> {
    let Some(t) = st.transport.lock().unwrap().as_ref().map(|t| t.sessions()) else {
        return Vec::new();
    };
    t.snapshot()
        .into_iter()
        .map(|(peer, entry)| GuestView {
            id: peer.iter().map(|b| format!("{b:02x}")).collect(),
            secs: entry.elapsed_secs(),
            nickname: entry.nickname,
            via_relay: entry.via_relay,
        })
        .collect()
}

#[tauri::command]
fn kick(st: State<'_, App>, id: String) -> Result<(), String> {
    let mut peer = [0u8; 32];
    if id.len() != 64 {
        return Err("认不出这个人".into());
    }
    for i in 0..32 {
        let byte = u8::from_str_radix(&id[i * 2..i * 2 + 2], 16).map_err(|_| "认不出这个人")?;
        peer[i] = byte;
    }
    if let Some(t) = st.transport.lock().unwrap().as_ref() {
        klog!("INFO", "踢人：{id}");
        t.sessions().kick(&peer);
    }
    Ok(())
}

// —— 自绘标题栏要用的三个动作。放在 Rust 里，界面不碰 window 权限 ——

#[tauri::command]
fn win_minimize(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.minimize();
    }
}

/// 窗口现在在哪。自己拖着走的时候要用它当起点。
#[tauri::command]
fn win_pos(app: AppHandle) -> (i32, i32) {
    app.get_webview_window("main")
        .and_then(|w| w.outer_position().ok())
        .map(|p| (p.x, p.y))
        .unwrap_or((0, 0))
}

/// 把窗口挪到 (x, y)。
///
/// 不用 `start_dragging()`：无边框窗口上它要靠系统的拖动循环，
/// 抬手快一点就丢帧，用户会感觉"拖不动"。自己算位移更听话。
#[tauri::command]
fn win_set_pos(app: AppHandle, x: i32, y: i32) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
    }
}

/// 关闭按钮 = 缩到托盘（门继续开着），和点窗口右上角一致
#[tauri::command]
fn win_hide(app: AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
}

/// 退出前收摊：让 transport 把房间从节点上摘掉，别留下挂到过期为止的空房。
fn shutdown(app: &AppHandle) {
    klog!("INFO", "退出：收拾房间");
    if let Some(state) = app.try_state::<App>() {
        if let Some(t) = state.transport.lock().unwrap().as_mut() {
            t.close();
        }
    }
    // 给 close 请求一点点时间落地；进程立刻退会把连接掐了
    std::thread::sleep(std::time::Duration::from_millis(250));
}

/// 把主窗口叫出来：显示 + 取消最小化 + 聚焦。
fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn main() {
    // 日志必须比别的东西先起来：后面任何一步出错都得留下痕迹。
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    // 日志跟设置放一起（%APPDATA%\cc.mcii.knock），不扔在 exe 旁边
    let log_dir = logging::default_dir();
    let log_file = logging::init(&log_dir);
    klog!("INFO", "Knock {} 启动", env!("CARGO_PKG_VERSION"));
    klog!("INFO", "程序目录：{}", exe_dir.display());
    klog!("INFO", "日志文件：{}", log_file.display());
    klog!("INFO", "系统：{} {}", std::env::consts::OS, std::env::consts::ARCH);

    // 崩了也要留一行。特别是 WebView2 起不来这种"双击没反应"的情况——
    // 光看窗口没出现，用户没法猜是上一份进程还没退干净。
    std::panic::set_hook(Box::new(|info| {
        let text = info.to_string();
        logging::write("ERROR", &format!("崩溃：{text}"));
        if text.contains("create webview") || text.contains("WebView2") {
            logging::write(
                "ERROR",
                "界面引擎（WebView2）没起来。多半是上一份 Knock 还没退干净：等几秒再开，或结束残留的 msedgewebview2.exe 再试。",
            );
        }
        let bt = format!("{:?}", std::backtrace::Backtrace::capture());
        if !bt.trim().is_empty() && !bt.contains("<disabled>") {
            logging::write("ERROR", &format!("调用栈：
{bt}"));
        }
    }));

    // 界面引擎被上一次残留的 msedgewebview2 锁住时（强制结束后很常见），
    // Tauri 会直接 panic。换数据目录这招试过——WebView2 照样报同一个共享冲突，
    // 所以这里不做无用功：panic hook 已经把"结束残留进程再试"写进日志了。
    launch();
}

/// 起界面。
fn launch() {
    tauri::Builder::default()
        // 记下到底加载的是打包进去的页面，还是开发服务器地址。
        .on_page_load(|_webview, payload| {
            klog!("INFO", "界面加载：{}", payload.url());
        })
        // 单实例：二次启动只把已有窗口叫出来，不开第二扇门
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_main(app);
        }))
        .manage(App::default())
        .invoke_handler(tauri::generate_handler![
            load_settings,
            save_settings,
            open_door,
            knock,
            close_door,
            door_state,
            door_code,
            entry_addr,
            nodes_cached,
            nodes_refresh,
            nodes_probe,
            node_url_from_code,
            guests,
            kick,
            log_path,
            open_log,
            app_version,
            win_minimize,
            win_pos,
            win_set_pos,
            win_hide
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // setup 时窗口还是 about:blank，真地址要等加载完才看得到，
            // 所以用 Builder 的 on_page_load 钩子（见下面 builder 链）。
            let _ = &handle;

            let open = MenuItem::with_id(app, "open", "打开主界面", true, None::<&str>)?;
            let close_door_item = MenuItem::with_id(app, "close_door", "关门", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &close_door_item, &quit])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().expect("图标没打进去").clone())
                .tooltip("Knock · 联机工具")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "open" => {
                        klog!("INFO", "托盘：打开主界面");
                        show_main(app);
                    }
                    "close_door" => {
                        klog!("INFO", "托盘：关门");
                        // 关门要真的关：先叫界面，再让它调 close_door 命令
                        let _ = app.emit("knock://tray", "close_door");
                        show_main(app);
                    }
                    "quit" => {
                        klog!("INFO", "托盘：退出");
                        shutdown(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            // 关窗口缩到托盘，门继续开着
            if let Some(window) = app.get_webview_window("main") {
                let w = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = w.hide();
                    }
                });
            }

            let _ = handle;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Knock 启动失败");
}
