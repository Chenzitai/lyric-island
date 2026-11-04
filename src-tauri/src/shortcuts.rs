//! 全局快捷键 —— 最小集，默认关闭。
//!
//! 为什么默认关闭：全局快捷键会抢占别的软件，装完就生效是流氓行为。
//! 用户要在设置里显式打开，打开时按当前键位注册，关掉立刻全部注销。
//!
//! 注册失败（被别的程序占用）**不 panic**，把失败项名字收集起来返回给
//! 设置页，让用户看到"Ctrl+Shift+L 已被占用"而不是无声无息。

use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Runtime};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// 一次注册尝试的结果
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutReport {
    pub registered: Vec<String>,
    pub failed: Vec<String>,
}

static CURRENT: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn toggle_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyL)
}

fn offset_up_shortcut() -> Shortcut {
    // 歌词延后 200ms
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::ArrowRight)
}

fn offset_down_shortcut() -> Shortcut {
    // 歌词提前 200ms
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::ArrowLeft)
}

fn picker_shortcut() -> Shortcut {
    // 打开「选择歌词」窗口
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyP)
}

fn names() -> Vec<(&'static str, Shortcut)> {
    vec![
        ("Ctrl+Shift+L", toggle_shortcut()),
        ("Ctrl+Shift+→", offset_up_shortcut()),
        ("Ctrl+Shift+←", offset_down_shortcut()),
        ("Ctrl+Shift+P", picker_shortcut()),
    ]
}

/// 注册全部快捷键。已经注册过的先注销，避免重复注册报错。
pub fn register_all<R: Runtime>(app: &AppHandle<R>) -> ShortcutReport {
    unregister_all(app);

    let mut registered = Vec::new();
    let mut failed = Vec::new();

    for (label, sc) in names() {
        // 事件处理器：只管按下，不管松开
        let handler_app = app.clone();
        let res = app.global_shortcut().on_shortcut(sc, move |_app, shortcut, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let app: &AppHandle<R> = &handler_app;
            let s = crate::settings::get();

            // 用 keycode 区分三个快捷键
            match shortcut.key {
                Code::KeyL => {
                    let mut cfg = s.clone();
                    cfg.island_visible = !cfg.island_visible;
                    let visible = cfg.island_visible;
                    crate::settings::apply(app, cfg);
                    crate::settings::apply_island_visible(app, visible);
                }
                Code::ArrowRight => {
                    let mut cfg = s.clone();
                    cfg.lyric_offset_ms = cfg.lyric_offset_ms.saturating_add(200);
                    crate::settings::apply(app, cfg.clone());
                    let _ = app.emit_to("island", "lyric:offset", cfg.lyric_offset_ms);
                }
                Code::ArrowLeft => {
                    let mut cfg = s.clone();
                    cfg.lyric_offset_ms = cfg.lyric_offset_ms.saturating_sub(200);
                    crate::settings::apply(app, cfg.clone());
                    let _ = app.emit_to("island", "lyric:offset", cfg.lyric_offset_ms);
                }
                Code::KeyP => {
                    crate::open_lyric_picker_impl(app);
                }
                _ => {}
            }
            let _ = app.emit_to("settings", "shortcut:fired", ());
        });

        match res {
            Ok(()) => registered.push(label.to_string()),
            Err(e) => {
                eprintln!("[shortcut] 注册 {label} 失败：{e}");
                failed.push(label.to_string());
            }
        }
    }

    if let Ok(mut g) = CURRENT.lock() {
        *g = registered.clone();
    }

    ShortcutReport { registered, failed }
}

/// 注销全部。设置里关掉总开关、退出前都要调。
pub fn unregister_all<R: Runtime>(app: &AppHandle<R>) {
    if let Err(e) = app.global_shortcut().unregister_all() {
        eprintln!("[shortcut] 注销失败：{e}");
    }
    if let Ok(mut g) = CURRENT.lock() {
        g.clear();
    }
}

/// 根据配置决定注册还是注销
pub fn sync_with_config<R: Runtime>(app: &AppHandle<R>) -> ShortcutReport {
    let cfg = crate::settings::get();
    if cfg.shortcuts_enabled {
        register_all(app)
    } else {
        unregister_all(app);
        ShortcutReport {
            registered: Vec::new(),
            failed: Vec::new(),
        }
    }
}

/// 设置页调这个：开关 + 立刻生效 + 返回注册结果
#[tauri::command]
pub async fn set_shortcuts_enabled(
    app: AppHandle,
    enabled: bool,
) -> Result<ShortcutReport, String> {
    let mut s = crate::settings::get();
    s.shortcuts_enabled = enabled;
    crate::settings::apply(&app, s);

    let report = sync_with_config(&app);
    if !report.failed.is_empty() {
        eprintln!("[shortcut] 以下快捷键被占用：{:?}", report.failed);
    }
    Ok(report)
}

#[tauri::command]
pub fn get_shortcut_status() -> ShortcutReport {
    let registered = CURRENT.lock().map(|g| g.clone()).unwrap_or_default();
    let cfg = crate::settings::get();
    let all: Vec<String> = names().into_iter().map(|(l, _)| l.to_string()).collect();
    let failed = if cfg.shortcuts_enabled {
        all.into_iter().filter(|n| !registered.contains(n)).collect()
    } else {
        Vec::new()
    };
    ShortcutReport { registered, failed }
}
