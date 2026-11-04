//! 桌面歌词窗口。
//!
//! 和岛体的区别：
//!   - 岛体是"一次性看一句"，桌面歌词是"常驻在桌面上看多行"；
//!   - 岛体必须能点（有悬停面板），桌面歌词默认**穿透**，点得穿到下层软件。
//!
//! 锁定的实现是两件事一起做：
//!   1. `set_ignore_cursor_events(true)` —— 鼠标事件不派发给这个窗口；
//!   2. `set_focusable(false)` —— 连激活都不给。
//! 只做第 1 条的话，某些情况下第一下点击仍会被窗口吞掉（Windows 会先激活再转交），
//! 用户感受到的就是"点一下没反应，要点两下"。
//!
//! 位置存的是**逻辑坐标**。换显示器或改缩放后，用新的 scale 重算物理位置，
//! 否则窗口会跑到屏幕外或被拉变形。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, LogicalPosition, Manager, Runtime, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "desktop-lyrics";

/// 上次落盘位置的时刻。拖动时 `Moved` 事件会很密集，
/// 每次都写 JSON 太浪费，1.2 秒内的重复移动只更新内存。
static LAST_SAVE: Mutex<Option<Instant>> = Mutex::new(None);

/// 按字号算窗口尺寸。字号越大，窗口跟着长高、变宽。
fn size_for(font_size: f64) -> (f64, f64) {
    let font = font_size.clamp(16.0, 72.0);
    let w = (font * 24.0).clamp(560.0, 1180.0);
    let h = (font * 2.9).clamp(78.0, 230.0);
    (w, h)
}

/// 创建（或复用）桌面歌词窗口。
///
/// 注意这里**不**根据 `desktop_lyrics_visible` 决定显示与否 —— 建窗口和
/// 显示是两件事，调用方自己再 show/hide。这样才能做到"隐藏后仍保留
/// 内部状态（当前行、滚动位置）"，再显示时不会闪一下。
pub fn build<R: Runtime>(app: &AppHandle<R>) -> Option<WebviewWindow<R>> {
    if let Some(w) = app.get_webview_window(LABEL) {
        return Some(w);
    }

    let cfg = crate::settings::get();
    let (w, h) = size_for(cfg.desktop_lyrics_font_size);

    let built = WebviewWindowBuilder::new(
        app,
        LABEL,
        WebviewUrl::App("index.html?window=desktop-lyrics".into()),
    )
    .title("桌面歌词")
    .inner_size(w, h)
    .decorations(false)
    .transparent(true)
    // 不要 shadow：透明窗口加阴影，Windows 会在边上补一圈黑，
    // 在浅色壁纸上非常明显。
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .resizable(false)
    // 创建时先不可聚焦，锁定态是默认值
    .focusable(!cfg.desktop_lyrics_locked)
    .visible(false)
    .devtools(true)
    .build();

    let win = match built {
        Ok(win) => win,
        Err(e) => {
            eprintln!("[desktop-lyrics] 创建窗口失败：{e}");
            return None;
        }
    };

    // 位置：有存过的就用存的，没有就贴在屏幕下方三分之一处
    place(&win, cfg.desktop_lyrics_pos);
    apply_locked(&win, cfg.desktop_lyrics_locked);

    Some(win)
}

/// 把窗口摆到指定逻辑坐标；坐标为空或越界时用默认位置。
fn place<R: Runtime>(win: &WebviewWindow<R>, pos: Option<[f64; 2]>) {
    let monitor = win
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| win.primary_monitor().ok().flatten());

    let (scale, origin, size) = match monitor {
        Some(m) => {
            let s = m.scale_factor();
            (
                s,
                m.position().to_logical::<f64>(s),
                m.size().to_logical::<f64>(s),
            )
        }
        None => (1.0, LogicalPosition::new(0.0, 0.0), tauri::LogicalSize::new(1920.0, 1080.0)),
    };

    let win_size = win
        .outer_size()
        .map(|s| s.to_logical::<f64>(scale))
        .unwrap_or(tauri::LogicalSize::new(560.0, 90.0));

    let (x, y) = match pos {
        Some([x, y]) => {
            // 存过坐标也要夹一下：可能用户在这之后把显示器改了分辨率
            let max_x = origin.x + size.width - win_size.width;
            let max_y = origin.y + size.height - win_size.height;
            (x.clamp(origin.x, max_x.max(origin.x)), y.clamp(origin.y, max_y.max(origin.y)))
        }
        None => {
            let x = origin.x + (size.width - win_size.width) / 2.0;
            let y = origin.y + size.height * 0.72;
            (x, y)
        }
    };

    let _ = win.set_position(LogicalPosition::new(x, y));
}

/// 应用锁定状态。锁定 = 鼠标穿透 + 不可聚焦。
pub fn apply_locked<R: Runtime>(win: &WebviewWindow<R>, locked: bool) {
    let _ = win.set_ignore_cursor_events(locked);
    let _ = win.set_focusable(!locked);
    // 解锁时给个轻微的高亮提示：置顶属性顺手再确认一次
    let _ = win.set_always_on_top(true);
}

/// 显示 / 隐藏桌面歌词窗口。
pub fn apply_visible<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    if visible {
        let win = match build(app) {
            Some(w) => w,
            None => return,
        };
        let cfg = crate::settings::get();
        let (w, h) = size_for(cfg.desktop_lyrics_font_size);
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
        apply_locked(&win, cfg.desktop_lyrics_locked);
        let _ = win.show();
        // 显示之后不要再抢焦点 —— 焦点在播放器上是正常的
        let _ = win.set_focusable(!cfg.desktop_lyrics_locked);
    } else if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.hide();
    }
}

/// 记录窗口新位置（逻辑坐标）。带 1.2 秒节流，避免拖动时疯狂写盘。
pub fn remember_position<R: Runtime>(app: &AppHandle<R>, win: &WebviewWindow<R>) {
    // 窗口还没显示时不要记。
    //
    // 坑：Tauri 新建的窗口会先落在"上一个窗口的位置"上（同进程复用），
    // 我们随后用 `place()` 把它挪到正确位置，但那次初始摆放也会触发一次
    // Moved 事件。如果不挡住，存下来的是岛体顶部那个坐标，
    // 下次桌面歌词就会诡异地出现在屏幕顶端。
    if !win.is_visible().unwrap_or(false) {
        return;
    }

    let scale = win.scale_factor().unwrap_or(1.0);
    let pos = match win.outer_position() {
        Ok(p) => p.to_logical::<f64>(scale),
        Err(_) => return,
    };

    {
        let mut last = match LAST_SAVE.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if let Some(t) = *last {
            if t.elapsed() < Duration::from_millis(1200) {
                // 只更新内存，等安静下来再一起落盘
                let mut cfg = crate::settings::get();
                cfg.desktop_lyrics_pos = Some([pos.x, pos.y]);
                crate::settings::set_in_memory(cfg);
                return;
            }
        }
        *last = Some(Instant::now());
    }

    let mut cfg = crate::settings::get();
    cfg.desktop_lyrics_pos = Some([pos.x, pos.y]);
    crate::settings::apply(app, cfg);
}

// ============================================================
// 命令
// ============================================================

/// 显示 / 隐藏桌面歌词（托盘菜单、设置页都走这条）。
#[tauri::command]
pub async fn set_desktop_lyrics_visible(app: AppHandle, visible: bool) -> Result<bool, String> {
    let mut cfg = crate::settings::get();
    cfg.desktop_lyrics_visible = visible;
    crate::settings::apply(&app, cfg);
    apply_visible(&app, visible);
    // 告诉跟随状态机"这是用户手动关的"，别在下一帧又给弹回来
    crate::follow::note_manual(visible);
    Ok(visible)
}

/// 切换锁定态。锁定时窗口点不中（能穿透到下层软件）。
#[tauri::command]
pub async fn set_desktop_lyrics_locked(app: AppHandle, locked: bool) -> Result<bool, String> {
    let mut cfg = crate::settings::get();
    cfg.desktop_lyrics_locked = locked;
    crate::settings::apply(&app, cfg);
    if let Some(win) = app.get_webview_window(LABEL) {
        apply_locked(&win, locked);
    }
    Ok(locked)
}

/// 改字号：窗口尺寸要跟着变。
#[tauri::command]
pub async fn set_desktop_lyrics_font(app: AppHandle, size: f64) -> Result<f64, String> {
    let size = size.clamp(16.0, 72.0);
    let mut cfg = crate::settings::get();
    cfg.desktop_lyrics_font_size = size;
    crate::settings::apply(&app, cfg);

    if let Some(win) = app.get_webview_window(LABEL) {
        let (w, h) = size_for(size);
        let _ = win.set_size(tauri::LogicalSize::new(w, h));
    }
    Ok(size)
}

/// 拖动结束后由前端调用，保存最终位置。
#[tauri::command]
pub fn save_desktop_pos(app: AppHandle, x: f64, y: f64) {
    let mut cfg = crate::settings::get();
    cfg.desktop_lyrics_pos = Some([x, y]);
    crate::settings::apply(&app, cfg);
}

/// 跟随播放自动显示桌面歌词。
///
/// 和 `apply_visible(app, true)` 的区别只有两点：
///   1. **绝不改 `desktop_lyrics_visible`** —— 那是用户的持久化意图，
///      自动行为不该污染它（否则"我手动关掉了它又自己冒出来"）；
///   2. 显示之后再兜一次 `set_focusable(false)`，确保锁定态下
///      绝不可能把焦点从用户正在用的软件上抢走。
pub fn auto_show<R: Runtime>(app: &AppHandle<R>) {
    let cfg = crate::settings::get();
    apply_visible(app, true);
    if let Some(win) = app.get_webview_window(LABEL) {
        // 锁定时窗口带 WS_EX_NOACTIVATE，show() 本来就不会激活它；
        // 这里再设一次是防"用户解锁了桌面歌词又开了跟随播放"的情况。
        if cfg.desktop_lyrics_locked {
            let _ = win.set_focusable(false);
        }
        // 跟随播放时永远置顶，否则被别的窗口盖住就白弹了
        let _ = win.set_always_on_top(true);
    }
    eprintln!("[desktop-lyrics] 跟随播放：自动显示");
}

/// 跟随播放自动隐藏（同样不碰持久化配置）。
pub fn auto_hide<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.hide();
        eprintln!("[desktop-lyrics] 跟随播放：自动隐藏");
    }
}

/// 调试用：读窗口几何信息，验证"锁定 = 穿透 + 不可聚焦"是否真的生效。
/// 仅 dev 构建可用（release 编译时整个函数被摘掉，也不会出现在命令表里）。
#[cfg(debug_assertions)]
#[tauri::command]
pub fn debug_desktop_lyrics(app: AppHandle) -> serde_json::Value {
    let cfg = crate::settings::get();
    match app.get_webview_window(LABEL) {
        Some(w) => {
            let scale = w.scale_factor().unwrap_or(1.0);
            let pos = w.outer_position().map(|p| p.to_logical::<f64>(scale)).ok();
            let size = w.outer_size().map(|s| s.to_logical::<f64>(scale)).ok();
            serde_json::json!({
                "exists": true,
                "visible": w.is_visible().unwrap_or(false),
                "locked": cfg.desktop_lyrics_locked,
                // Tauri 2 的 WebviewWindow 没有 is_focusable() 查询接口，
                // 只能按"锁定即不可聚焦"这个约定回读配置
                "focusable": !cfg.desktop_lyrics_locked,
                "x": pos.map(|p| p.x),
                "y": pos.map(|p| p.y),
                "w": size.map(|s| s.width),
                "h": size.map(|s| s.height),
                "fontSize": cfg.desktop_lyrics_font_size,
            })
        }
        None => serde_json::json!({ "exists": false, "locked": cfg.desktop_lyrics_locked }),
    }
}
