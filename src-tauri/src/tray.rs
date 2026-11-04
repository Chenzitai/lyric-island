//! 系统托盘 —— 唯一常驻入口。
//!
//! 因为岛体是 `skip_taskbar: true` 的透明窗口，托盘是用户唯一能"找到"它的地方。
//! 没有托盘的话，关掉窗口就只能去任务管理器结束进程。
//!
//! 菜单每次刷新都整个重建。听起来浪费，但 Tauri 没有"菜单即将打开"事件，
//! 动态子菜单（会话列表）只能靠重建。2 秒一次，开销可以忽略。

use std::sync::Mutex;

use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuItem, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Runtime};

use crate::{settings, smtc};

pub const TRAY_ID: &str = "lyric-tray";

/// 上一次成功设置到托盘上的菜单内容签名。
///
/// 每 2 秒无条件重建是错的：用户右键按下的瞬间菜单被替换掉，
/// Windows 就没法把点击转发给旧菜单，表现就是"右键没反应"。
/// release 版实测点不出来，dev 偶尔能出来，差别只在时机。
static LAST_SIG: Mutex<Option<String>> = Mutex::new(None);

fn menu_signature() -> String {
    let cfg = settings::get();
    let np = smtc::last_now_playing();
    let sessions = smtc::last_sessions();
    let mut app_ids: Vec<String> = sessions.iter().map(|s| s.app_id.clone()).collect();
    app_ids.sort();
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}|{}",
        cfg.follow_app.clone().unwrap_or_default(),
        cfg.lyric_offset_ms,
        cfg.island_visible,
        cfg.desktop_lyrics_visible,
        cfg.desktop_lyrics_locked,
        np.active,
        np.title,
        np.artist,
        app_ids.join(",")
    )
}

/// 建托盘。失败只记日志 —— 托盘挂了不该让整个应用起不来。
pub fn build<R: Runtime>(app: &AppHandle<R>) {
    let menu = match make_menu(app) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[tray] 构建菜单失败：{e}");
            return;
        }
    };

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        // 左键不弹菜单，改成切换显隐（自己在 on_tray_icon_event 里接管）
        .show_menu_on_left_click(false)
        .tooltip(tooltip_text())
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_tray_event);

    // 托盘必须有图标，否则 Windows 会建出一个看不见的空位。
    let has_default = app.default_window_icon().is_some();
    eprintln!("[tray] default_window_icon = {has_default}");
    if let Some(icon) = app.default_window_icon().cloned() {
        builder = builder.icon(icon);
    }

    match builder.build(app) {
        Ok(t) => eprintln!("[tray] 已创建 id={:?}", t.id().as_ref()),
        Err(e) => eprintln!("[tray] 创建托盘图标失败：{e}"),
    }
    // 记下初始签名，免得第一次 refresh 立刻又重建一遍
    if let Ok(mut g) = LAST_SIG.lock() {
        *g = Some(menu_signature());
    }
}

/// 刷新菜单文本和 tooltip（SMTC 线程每 2 秒调一次）
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    let sig = menu_signature();
    let changed = match LAST_SIG.lock() {
        Ok(mut g) => {
            if g.as_deref() == Some(sig.as_str()) {
                false
            } else {
                *g = Some(sig);
                true
            }
        }
        Err(_) => true,
    };

    if changed {
        if let Ok(menu) = make_menu(app) {
            let _ = tray.set_menu(Some(menu));
        }
    }
    let _ = tray.set_tooltip(Some(tooltip_text()));
}

fn tooltip_text() -> String {
    let np = smtc::last_now_playing();
    if np.active && !np.title.is_empty() {
        if np.artist.trim().is_empty() {
            np.title
        } else {
            format!("{} - {}", np.title, np.artist)
        }
    } else {
        "Lyric Island".into()
    }
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        s.to_string()
    } else {
        format!("{}…", chars[..max].iter().collect::<String>())
    }
}

fn make_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let cfg = settings::get();
    let np = smtc::last_now_playing();
    let sessions = smtc::last_sessions();

    // ---- 显示 / 隐藏 ----
    let toggle = CheckMenuItem::with_id(
        app,
        "toggle",
        "显示灵动岛",
        true,
        cfg.island_visible,
        None::<&str>,
    )?;

    // ---- 当前跟随 ----
    let current_label = if np.active && !np.title.is_empty() {
        let app_name = np.app_id.rsplit('\\').next().unwrap_or(&np.app_id);
        format!("当前跟随：{} · {}", truncate(app_name, 18), truncate(&np.title, 20))
    } else {
        "当前跟随：（无会话）".to_string()
    };
    let current_item = MenuItem::with_id(app, "noop", current_label, false, None::<&str>)?;

    // 跟随子菜单：自动 + 每个会话一项
    let auto_checked = cfg.follow_app.is_none();
    let mut follow = SubmenuBuilder::new(app, "跟随指定播放器");
    let auto_item = MenuItem::with_id(
        app,
        "follow:auto",
        if auto_checked { "✓ 自动选择" } else { "自动选择" },
        !auto_checked,
        None::<&str>,
    )?;
    follow = follow.item(&auto_item);

    for s in sessions.iter().take(8) {
        let id = format!("follow:{}", s.app_id);
        let picked = cfg
            .follow_app
            .as_deref()
            .map(|f| f.eq_ignore_ascii_case(&s.app_id))
            .unwrap_or(false);
        let label = if picked {
            format!("✓ {} · {}", truncate(&s.app_id, 20), truncate(&s.title, 16))
        } else {
            format!("{} · {}", truncate(&s.app_id, 20), truncate(&s.title, 16))
        };
        let item = MenuItem::with_id(app, id, label, !picked, None::<&str>)?;
        follow = follow.item(&item);
    }
    let follow_menu = follow.build()?;

    // ---- 歌词偏移 ----
    let offset_item = MenuItem::with_id(
        app,
        "noop2",
        format!("歌词偏移：{:+} ms", cfg.lyric_offset_ms),
        false,
        None::<&str>,
    )?;
    let mut offset_menu = SubmenuBuilder::new(app, "调整歌词偏移");
    for (id, label) in [
        ("offset:-500", "提前 500ms"),
        ("offset:-100", "提前 100ms"),
        ("offset:0", "归零"),
        ("offset:100", "延后 100ms"),
        ("offset:500", "延后 500ms"),
    ] {
        let item = MenuItem::with_id(app, id, label, true, None::<&str>)?;
        offset_menu = offset_menu.item(&item);
    }
    let offset_sub = offset_menu.build()?;

    // ---- 歌词对齐（只在播放器不提供时间轴时可用）----
    // 有时间轴说明进度是真的，再手动微调只会越调越偏，所以整组置灰。
    let align_enabled = np.active && !np.has_timeline;
    let align_hint = MenuItem::with_id(
        app,
        "noop3",
        if !np.active {
            "对齐：无会话"
        } else if np.has_timeline {
            "对齐：精确时间轴"
        } else {
            "对齐：估算（可微调）"
        },
        false,
        None::<&str>,
    )?;
    let mut align_menu = SubmenuBuilder::new(app, "歌词对齐");
    for (id, label) in [
        ("align:prev", "上一句"),
        ("align:next", "下一句"),
        ("align:-5000", "后退 5 秒"),
        ("align:5000", "前进 5 秒"),
    ] {
        let item = MenuItem::with_id(app, id, label, align_enabled, None::<&str>)?;
        align_menu = align_menu.item(&item);
    }
    let align_sub = align_menu.build()?;

    // ---- 其它 ----
    // 桌面歌词开关放在这里：它是个"常驻显示"的窗口，比歌词偏移更常用
    let desktop_item = CheckMenuItem::with_id(
        app,
        "desktop-lyrics",
        "桌面歌词",
        true,
        cfg.desktop_lyrics_visible,
        None::<&str>,
    )?;
    let full_lyrics_item =
        MenuItem::with_id(app, "full-lyrics", "完整歌词…", true, None::<&str>)?;
    let picker_item = MenuItem::with_id(app, "picker", "选择歌词…", true, None::<&str>)?;
    let settings_item = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;

    let mb = MenuBuilder::new(app)
        .item(&toggle)
        .item(&desktop_item)
        .separator()
        .item(&current_item)
        .item(&follow_menu)
        .separator()
        .item(&offset_item)
        .item(&offset_sub)
        .item(&align_hint)
        .item(&align_sub)
        .separator()
        .item(&full_lyrics_item)
        .item(&picker_item)
        .item(&settings_item)
        .item(&quit_item);

    mb.build()
}

fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref().to_string();

    match id.as_str() {
        "toggle" => {
            let mut s = settings::get();
            s.island_visible = !s.island_visible;
            let visible = s.island_visible;
            settings::apply(app, s);
            settings::apply_island_visible(app, visible);
            refresh(app);
        }
        "follow:auto" => {
            let mut s = settings::get();
            s.follow_app = None;
            settings::apply(app, s);
            refresh(app);
        }
        "picker" => {
            crate::open_lyric_picker_impl(app);
        }
        "desktop-lyrics" => {
            let mut s = settings::get();
            s.desktop_lyrics_visible = !s.desktop_lyrics_visible;
            let visible = s.desktop_lyrics_visible;
            settings::apply(app, s);
            crate::desktop_lyrics::apply_visible(app, visible);
            refresh(app);
        }
        "full-lyrics" => {
            crate::full_lyrics::open(app);
        }
        other if other.starts_with("align:") => {
            // 具体的行/毫秒换算交给前端：只有前端知道当前歌词行的时间戳
            let arg = other.trim_start_matches("align:");
            let payload = match arg {
                "prev" | "next" => serde_json::json!({ "mode": arg }),
                ms => serde_json::json!({ "deltaMs": ms.parse::<i64>().unwrap_or(0) }),
            };
            let _ = app.emit("lyric:align", payload);
        }
        "settings" => {
            settings::open_settings_window(app);
        }
        "quit" => {
            app.exit(0);
        }
        other if other.starts_with("follow:") => {
            let app_id = other.trim_start_matches("follow:").to_string();
            let mut s = settings::get();
            s.follow_app = Some(app_id);
            settings::apply(app, s);
            refresh(app);
        }
        other if other.starts_with("offset:") => {
            let delta: i32 = other.trim_start_matches("offset:").parse().unwrap_or(0);
            let mut s = settings::get();
            s.lyric_offset_ms = delta;
            settings::apply(app, s);
            // 通知岛体立刻生效，不用等下一帧设置广播
            let _ = tauri::Emitter::emit_to(app, "island", "lyric:offset", delta);
            refresh(app);
        }
        _ => {}
    }
}

fn on_tray_event<R: Runtime>(tray: &tauri::tray::TrayIcon<R>, event: TrayIconEvent) {
    // 左键单击切换显隐；右键留给菜单
    if let TrayIconEvent::Click {
        button: MouseButton::Left,
        button_state: MouseButtonState::Up,
        ..
    } = event
    {
        let app = tray.app_handle();
        let mut s = settings::get();
        s.island_visible = !s.island_visible;
        let visible = s.island_visible;
        settings::apply(app, s);
        settings::apply_island_visible(app, visible);
        refresh(app);
    }
}
