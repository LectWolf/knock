//! 桌面壳：主窗口 + 托盘 + 界面命令。
//!
//! 分工：
//!   - 网络与状态机全在 `knock` 库里，这里**不判断连接逻辑**
//!   - 这个文件只做三件事：装配 transport、把 `Update` 推给界面、响应托盘

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
    settings::load(&config_dir(&app))
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<(), String> {
    settings::save(&config_dir(&app), &settings).map_err(|e| e.to_string())
}

/// 装配一次 transport：同一个节点、同一套状态回调。
fn build_transport(app: &AppHandle, s: &Settings) -> Result<IrohTransport, String> {
    if s.node.trim().is_empty() {
        return Err("先添加一个节点".into());
    }
    let client = Arc::new(HttpRoomClient::new(s.node.clone()));
    let mut transport = IrohTransport::new(client);

    // 状态只在 transport 内部迁移；这里只负责搬给界面
    let handle = app.clone();
    transport.on_state(Box::new(move |u: &Update| {
        let payload = StatePayload {
            state: state_name(u.state),
            fail: u.fail.map(fail_name),
            relay_offer: u.relay_offer,
        };
        let _ = handle.emit("knock://state", payload);
    }));
    Ok(transport)
}

#[tauri::command]
fn open_door(app: AppHandle, st: State<'_, App>, port: u16) -> Result<(), String> {
    let s = settings::load(&config_dir(&app));
    let mut transport = build_transport(&app, &s)?;
    transport
        .connect(Target::Open { port })
        .map_err(|e| format!("{e:?}"))?;
    *st.transport.lock().unwrap() = Some(transport);
    Ok(())
}

#[tauri::command]
fn knock(app: AppHandle, st: State<'_, App>, invite: String) -> Result<(), String> {
    let s = settings::load(&config_dir(&app));
    let mut transport = build_transport(&app, &s)?;
    transport
        .connect(Target::Knock { invite: &invite })
        .map_err(|e| format!("{e:?}"))?;
    *st.transport.lock().unwrap() = Some(transport);
    Ok(())
}

#[tauri::command]
fn close_door(st: State<'_, App>) {
    if let Some(t) = st.transport.lock().unwrap().as_mut() {
        t.close();
    }
    *st.transport.lock().unwrap() = None;
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

/// 把主窗口叫出来：显示 + 取消最小化 + 聚焦。
fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
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
            guests,
            kick,
            win_minimize,
            win_pos,
            win_set_pos,
            win_hide
        ])
        .setup(|app| {
            let handle = app.handle().clone();

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
                    "open" => show_main(app),
                    "close_door" => {
                        // 关门要真的关：先叫界面，再让它调 close_door 命令
                        let _ = app.emit("knock://tray", "close_door");
                        show_main(app);
                    }
                    "quit" => app.exit(0),
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
