mod clock;
mod desktop_lyrics;
mod follow;
mod full_lyrics;
mod lyrics;
mod settings;
mod shortcuts;
mod smtc;
mod tray;
mod wininfo;

use tauri::{Emitter, Manager, WindowEvent};

/// 把岛体重新摆到屏幕顶部居中。
///
/// 由前端在每次 resize 之后调用 —— 宽度变了，居中的 x 也要跟着变，
/// 否则岛会像左或右偏。位置计算放 Rust 侧是因为 JS 侧的显示器 API
/// 在不同小版本间位置不稳定。
#[tauri::command]
fn recenter_island(window: tauri::WebviewWindow, width: f64) -> Result<(), String> {
    let monitor = match window.current_monitor() {
        Ok(Some(m)) => m,
        _ => window
            .primary_monitor()
            .map_err(|e| format!("拿不到显示器信息：{e}"))?
            .ok_or_else(|| "没有可用显示器".to_string())?,
    };

    let scale = monitor.scale_factor();
    let screen = monitor.size().to_logical::<f64>(scale);
    let origin = monitor.position().to_logical::<f64>(scale);

    let x = origin.x + ((screen.width - width) / 2.0).max(0.0);
    window
        .set_position(tauri::LogicalPosition::new(x, origin.y + 8.0))
        .map_err(|e| format!("设置窗口位置失败：{e}"))
}

/// 记录上一次「居中摆放」时的 x（逻辑坐标）。
///
/// 用它来判断岛体当前是不是还在默认居中位置：
/// 如果在，宽度变化就重新居中；如果已经被拖走，就只做边缘夹取，
/// 免得用户刚拖到右下角，一展开就被拽回屏幕中间。
static LAST_CENTER_X: std::sync::Mutex<Option<f64>> = std::sync::Mutex::new(None);

/// 摆放岛体：居中态重新居中，非居中态只做**边缘碰撞**夹取。
///
/// 边缘碰撞就是用户要的那条：面板靠近屏幕左右边缘时，
/// 展开不能超出屏幕，要自动往内侧对齐。
#[tauri::command]
fn place_island(window: tauri::WebviewWindow, width: f64) -> Result<(), String> {
    let monitor = match window.current_monitor() {
        Ok(Some(m)) => m,
        _ => window
            .primary_monitor()
            .map_err(|e| format!("拿不到显示器信息：{e}"))?
            .ok_or_else(|| "没有可用显示器".to_string())?,
    };

    let scale = monitor.scale_factor();
    let screen = monitor.size().to_logical::<f64>(scale);
    let origin = monitor.position().to_logical::<f64>(scale);

    let default_x = origin.x + ((screen.width - width) / 2.0).max(0.0);

    // 当前窗口位置
    let cur_x = window
        .outer_position()
        .map(|p| p.to_logical::<f64>(scale).x)
        .unwrap_or(default_x);

    // 判断是不是"还待在默认居中位置"：
    // 拿当前 x 和上一次居中时的 x 比，差得多说明被拖过。
    let was_centered = match LAST_CENTER_X.lock() {
        Ok(g) => match *g {
            Some(prev) => (cur_x - prev).abs() < 48.0,
            // 第一次调用时还没有记录，按居中处理
            None => true,
        },
        Err(_) => true,
    };

    let target_x = if was_centered {
        default_x
    } else {
        // 边缘碰撞：左右都夹一下，宽度比屏幕还宽就以屏幕左边为准
        let min_x = origin.x;
        let max_x = (origin.x + screen.width - width).max(origin.x);
        cur_x.clamp(min_x, max_x)
    };

    if let Ok(mut g) = LAST_CENTER_X.lock() {
        *g = Some(default_x);
    }

    window
        .set_position(tauri::LogicalPosition::new(target_x, origin.y + 8.0))
        .map_err(|e| format!("设置窗口位置失败：{e}"))
}

#[tauri::command]
async fn open_settings(app: tauri::AppHandle) {
    settings::open_settings_window(&app);
}

/// 打开「选择歌词」窗口。
///
/// 这个窗口不复用设置窗口：选词是一次性任务，用完就该销毁，
/// 而且它需要比设置窗口更宽的结果列表。
pub fn open_lyric_picker_impl<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(w) = app.get_webview_window("lyric-picker") {
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let built = tauri::WebviewWindowBuilder::new(
        app,
        "lyric-picker",
        tauri::WebviewUrl::App("index.html?window=picker".into()),
    )
    .title("选择歌词")
    .inner_size(600.0, 660.0)
    .min_inner_size(480.0, 420.0)
    .center()
    .devtools(true)
    .build();

    match built {
        Ok(w) => {
            let _ = w.set_focus();
            eprintln!("[picker] 选词窗口已创建");
        }
        Err(e) => eprintln!("[picker] 打开选词窗口失败：{e}"),
    }
}

#[tauri::command]
async fn open_lyric_picker(app: tauri::AppHandle) {
    open_lyric_picker_impl(&app);
}

/// 调试用：把岛体当前的可视状态读出来，验证托盘/快捷键的显隐切换是否真的生效。
///
/// 岛体是 `skip_taskbar` 的透明窗口，从外部进程用 UIA 很难确认它到底是
/// "隐藏了" 还是 "显示着但全透明"，所以让 Rust 侧自己回答。
///
/// 只在 dev 构建里存在：`#[cfg(debug_assertions)]` 会在 release 编译时
/// 把函数定义和它的注册项一起摘掉，最终二进制里连这个命令名都不会留下。
#[cfg(debug_assertions)]
#[tauri::command]
fn debug_island_visible(app: tauri::AppHandle) -> bool {
    app.get_webview_window("island")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

/// 当前播放进度 —— 前端刷新/重连时调它，不要从 0 开始数。
///
/// 返回的是已经按播放状态外推过的位置：即使播放器 2 秒才报一次位置，
/// 这里给出的也是"现在这一刻"的估值。
#[tauri::command]
fn get_current_progress() -> clock::Progress {
    clock::progress()
}

/// 调试用：伪造一次 seek 事件，验证"Rust 判定 → 前端跟随"这一段。
/// 真实拖动进度条不好自动化，但这条路径的代码是同一份。仅 dev 构建可用。
#[cfg(debug_assertions)]
#[tauri::command]
fn debug_emit_seek(app: tauri::AppHandle, position_ms: f64) -> f64 {
    use tauri::Emitter;
    let _ = app.emit("smtc:seek", serde_json::json!({ "positionMs": position_ms }));
    position_ms
}

/// 调试用：等价于左键点托盘图标，验证那条路径。仅 dev 构建可用。
#[cfg(debug_assertions)]
#[tauri::command]
fn debug_toggle_island(app: tauri::AppHandle) -> bool {
    let visible = debug_island_visible(app.clone());
    settings::apply_island_visible(&app, !visible);
    !visible
}

// ---------------------------------------------------------------------------
// 命令表：release 里不带任何 debug_* 命令
// ---------------------------------------------------------------------------
//
// Tauri 的 `generate_handler!` 必须拿到一个完整的路径列表，没法在中间插
// `#[cfg]`，所以这里用两套宏：展开出来的就是两份不同的列表。
// release 构建里 debug_* 这几个标识符根本不会出现在宏展开结果中，
// 配合函数本身的 `#[cfg(debug_assertions)]`，最终二进制里彻底没有它们。

#[cfg(debug_assertions)]
macro_rules! app_handler {
    () => {
        tauri::generate_handler![
            lyrics::fetch_lyrics,
            lyrics::lyrics_status,
            lyrics::search_lyrics_candidates,
            lyrics::fetch_lyrics_for_candidate,
            lyrics::get_lyric_binding,
            lyrics::set_lyric_binding,
            lyrics::clear_lyric_binding,
            lyrics::list_lyric_bindings,
            smtc::list_sessions,
            smtc::list_windows,
            smtc::send_control,
            smtc::seek_to,
            smtc::now_playing,
            smtc::get_config,
            smtc::set_config,
            settings::get_settings,
            settings::set_settings,
            settings::patch_settings,
            settings::get_effective_offset,
            settings::get_autostart,
            settings::set_autostart,
            shortcuts::set_shortcuts_enabled,
            shortcuts::get_shortcut_status,
            recenter_island,
            place_island,
            open_settings,
            open_lyric_picker,
            full_lyrics::open_full_lyrics,
            desktop_lyrics::set_desktop_lyrics_visible,
            desktop_lyrics::set_desktop_lyrics_locked,
            desktop_lyrics::set_desktop_lyrics_font,
            desktop_lyrics::save_desktop_pos,
            desktop_lyrics::debug_desktop_lyrics,
            get_current_progress,
            debug_emit_seek,
            debug_island_visible,
            debug_toggle_island,
        ]
    };
}

#[cfg(not(debug_assertions))]
macro_rules! app_handler {
    () => {
        tauri::generate_handler![
            lyrics::fetch_lyrics,
            lyrics::lyrics_status,
            lyrics::search_lyrics_candidates,
            lyrics::fetch_lyrics_for_candidate,
            lyrics::get_lyric_binding,
            lyrics::set_lyric_binding,
            lyrics::clear_lyric_binding,
            lyrics::list_lyric_bindings,
            smtc::list_sessions,
            smtc::list_windows,
            smtc::send_control,
            smtc::seek_to,
            smtc::now_playing,
            smtc::get_config,
            smtc::set_config,
            settings::get_settings,
            settings::set_settings,
            settings::patch_settings,
            settings::get_effective_offset,
            settings::get_autostart,
            settings::set_autostart,
            shortcuts::set_shortcuts_enabled,
            shortcuts::get_shortcut_status,
            recenter_island,
            place_island,
            open_settings,
            open_lyric_picker,
            full_lyrics::open_full_lyrics,
            desktop_lyrics::set_desktop_lyrics_visible,
            desktop_lyrics::set_desktop_lyrics_locked,
            desktop_lyrics::set_desktop_lyrics_font,
            desktop_lyrics::save_desktop_pos,
            get_current_progress,
        ]
    };
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            // 自启时带这个参数，方便以后做"自启时不抢焦点/不弹设置"之类的分支
            Some(vec!["--autostart"]),
        ))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // ---- 1. 先读设置（后面所有初始化都依赖它）----
            let cfg = settings::load_from_store(app.handle());
            settings::set_in_memory(cfg.clone());

            // ---- 2. 开机自启用系统的真实状态覆盖 store ----
            // 用户可能去「任务管理器 → 启动」里手动禁用过，
            // 不覆盖的话设置页会显示"开着"但实际没生效。
            {
                use tauri_plugin_autostart::ManagerExt;
                if let Ok(actual) = app.autolaunch().is_enabled() {
                    if actual != cfg.launch_at_startup {
                        let mut s = settings::get();
                        s.launch_at_startup = actual;
                        settings::apply(app.handle(), s);
                    }
                }
            }

            // ---- 3. 岛体位置 / 尺寸 ----
            if let Some(win) = app.get_webview_window("island") {
                // 高 DPI 屏幕上 tauri.conf.json 里的 width/height 有可能被当成物理像素
                // （实测 200% 缩放下 420 配置 → 420 物理，而 WebView 按 dpr=2 渲染 840，
                // 结果只显示左上四分之一）。这里显式用 LogicalSize 再设一次，
                // 保证窗口物理尺寸 = 逻辑尺寸 × scaleFactor。
                let _ = win.set_size(tauri::LogicalSize::new(420.0_f64, 64.0_f64));

                if let Ok(Some(monitor)) = win.current_monitor().or_else(|_| win.primary_monitor()) {
                    let scale = monitor.scale_factor();
                    let size = monitor.size().to_logical::<f64>(scale);
                    let win_size = win.outer_size().map(|s| s.to_logical::<f64>(scale)).unwrap_or(
                        tauri::LogicalSize {
                            width: 420.0,
                            height: 64.0,
                        },
                    );
                    let x = (size.width - win_size.width) / 2.0;
                    let _ = win.set_position(tauri::LogicalPosition::new(x, 8.0));
                }
                // 不抢焦点：鼠标点上去也不该把当前窗口弄失去激活
                let _ = win.set_focusable(false);

                // 上次退出时如果是隐藏状态，这次也别弹出来
                if !cfg.island_visible {
                    let _ = win.hide();
                }
            }

            // ---- 3.5 桌面歌词窗口 ----
            // 只有上次是显示状态才建。不预先建的原因是：这个窗口置顶 + 透明，
            // 提前建出来容易在启动瞬间闪一下。
            if cfg.desktop_lyrics_visible {
                desktop_lyrics::apply_visible(app.handle(), true);
            }

            // ---- 4. 托盘（唯一常驻入口）----
            tray::build(app.handle());

            // ---- 5. 按配置注册快捷键 ----
            shortcuts::sync_with_config(app.handle());

            // ---- 6. 启动 SMTC 监听线程 ----
            smtc::spawn(app.handle().clone());

            Ok(())
        })
        .invoke_handler(app_handler!())
        .on_window_event(|window, event| {
            // 桌面歌词被拖动 → 记住新位置。
            // Moved 事件很密集（拖动时每帧都来），remember_position 内部有节流。
            if let WindowEvent::Moved(_) = event {
                if window.label() == desktop_lyrics::LABEL {
                    if let Some(w) = window
                        .app_handle()
                        .get_webview_window(desktop_lyrics::LABEL)
                    {
                        desktop_lyrics::remember_position(window.app_handle(), &w);
                    }
                }
                return;
            }

            if let WindowEvent::CloseRequested { api, .. } = event {
                let label = window.label();

                // 选词窗口 / 完整歌词窗口是"用完就关"的临时窗口，直接销毁
                if label == "lyric-picker" || label == full_lyrics::LABEL {
                    return;
                }

                // 桌面歌词：没有装饰没有关闭按钮，但万一被系统要求关闭，
                // 也只是隐藏（并同步设置），不要销毁窗口丢掉内部状态
                if label == desktop_lyrics::LABEL {
                    api.prevent_close();
                    let _ = window.hide();
                    let mut cfg = settings::get();
                    cfg.desktop_lyrics_visible = false;
                    settings::apply(window.app_handle(), cfg);
                    return;
                }

                // 岛体：没有关闭按钮，拦截掉，只隐藏
                // 设置窗口：关掉只是隐藏，下次点"设置…"直接显示，不用重建
                api.prevent_close();

                let app = window.app_handle();
                let is_island = label == "island";
                let first_close = is_island && !settings::get().tray_hint_shown;

                if first_close {
                    // 标记成"已提示过"并立刻落盘 —— 用户下次开机再关窗口不该再被打扰
                    let mut cfg = settings::get();
                    cfg.tray_hint_shown = true;
                    settings::apply(app, cfg);

                    // 主路径：让岛体自己显示一条提示，2.5 秒后再隐藏。
                    // 这条**一定看得见**：不依赖系统通知权限，也不会被
                    // Smart App Control / 通知免打扰拦掉。
                    let _ = app.emit_to(
                        "island",
                        "island:hint",
                        "已最小化到托盘，右键托盘图标可退出",
                    );

                    // 附加：顺手发一条系统通知，能弹就弹。
                    // 未打包/无 AppUserModelID 时常常弹不出来，所以失败静默 ——
                    // 提示职责由上面那条内联提示兜住了，这里不报错。
                    {
                        use tauri_plugin_notification::NotificationExt;
                        let _ = app
                            .notification()
                            .builder()
                            .title("Lyric Island 还在运行")
                            .body("已最小化到托盘，右键托盘图标可退出")
                            .show();
                    }

                    // 延迟隐藏，给提示留出显示时间。
                    let w = window.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(2500));
                        let _ = w.hide();
                    });
                } else {
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Tauri 启动失败");
}
