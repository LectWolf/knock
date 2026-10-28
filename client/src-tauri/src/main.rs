//! 桌面壳：主窗口 + 托盘。
//!
//! 所有网络逻辑在 `knock` 库里，这里只管窗——不碰 transport、不碰状态机。
//! 两条桌面端的硬约定：
//!   1. 关窗口 = 缩到托盘，门继续开着（不然"关掉窗口就会关门"这句界面文案就是骗人的）
//!   2. 真要退出只能走托盘菜单，避免手滑把人从服里踢下去

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, WindowEvent,
};

/// 把主窗口叫出来：显示 + 取消最小化 + 聚焦。
fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "打开主界面", true, None::<&str>)?;
            let close_door = MenuItem::with_id(app, "close_door", "关门", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &close_door, &quit])?;

            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .expect("图标没打进去")
                        .clone(),
                )
                .tooltip("Knock · 联机工具")
                .menu(&menu)
                // 左键点图标只想把窗口叫出来，不想弹菜单
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "close_door" => {
                        // 关门交给界面去做：真正的断连和关房间都在 transport 里
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
                let handle = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = handle.hide();
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Knock 启动失败");
}
