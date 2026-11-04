//! 设置的唯一权威。
//!
//! 原则：**Rust 侧是 source of truth**，前端只能通过命令读写。
//! 这样不会出现"前端以为开了、系统里其实没开"的分叉。
//!
//! 落盘用 `tauri-plugin-store`（JSON 文件，路径在 app_config_dir）。
//! 三个必须处理好的失败路径：
//!   1. 文件不存在 / 内容损坏 → 用默认值继续，**不能 unwrap 崩掉**；
//!   2. 每次修改立刻 save()，因为这东西经常被任务管理器直接杀掉，等退出时保存等于不保存；
//!   3. autostart 这类外部状态，启动时用系统的真实值回读，覆盖 store 里的旧值。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "settings.json";
const STORE_KEY: &str = "settings";

/// 全部配置项。`#[serde(default)]` 保证老版本文件缺字段也能读。
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    // ---- 跟随策略 ----
    /// 手动指定跟随哪个播放器（AUMID）。None = 自动打分
    pub follow_app: Option<String>,
    /// 排除的 AUMID 片段（不区分大小写，包含匹配）
    pub exclude_apps: Vec<String>,
    /// 是否允许视频会话
    pub allow_video: bool,
    /// 没有音乐会话时是否退回读窗口标题
    pub window_fallback: bool,
    /// 回退时只看这个进程
    pub window_process: String,
    /// 从窗口标题抠歌名的正则
    pub window_regex: String,

    // ---- 歌词 ----
    /// 全局歌词偏移（ms，正数 = 歌词延后）
    pub lyric_offset_ms: i32,
    /// 按应用覆盖偏移，如 {"kugou": 300}
    pub per_app_offset: HashMap<String, i32>,

    // ---- 界面 / 系统 ----
    /// 启动时是否显示岛体
    pub island_visible: bool,
    /// 开机自启
    pub launch_at_startup: bool,
    /// 全局快捷键总开关（默认关闭，opt-in）
    pub shortcuts_enabled: bool,
    /// 「关窗口只是最小化到托盘」这条提示是否已经弹过。
    /// 只弹一次，之后关闭窗口不再打扰。
    pub tray_hint_shown: bool,

    // ---- 悬停面板 / 显示 ----
    /// 第二行是否显示翻译 / 音译（关掉就只有原词）
    pub show_translation: bool,
    /// 翻译 / 音译的字号（px）
    pub translation_size: f64,
    /// 展开态底部是否显示进度条
    pub show_progress: bool,
    /// 暂停时把岛体收起来，只留一行歌词
    pub collapse_on_pause: bool,

    // ---- 桌面歌词 ----
    /// 桌面歌词窗口是否显示
    pub desktop_lyrics_visible: bool,
    /// 桌面歌词是否锁定。锁定时鼠标完全穿透（点得穿到下层软件）
    pub desktop_lyrics_locked: bool,
    /// 桌面歌词窗口位置，**逻辑坐标**。
    ///
    /// 存逻辑坐标而不是物理像素：换显示器 / 改缩放后，
    /// 用新的 scale 重算物理位置才不会跑到屏幕外。
    pub desktop_lyrics_pos: Option<[f64; 2]>,
    /// 桌面歌词字号（px，逻辑）
    pub desktop_lyrics_font_size: f64,
    /// 跟随播放自动显示/隐藏桌面歌词。
    ///
    /// 注意它只影响"自动行为"：值为 false 时，用户手动在托盘里打开的
    /// 桌面歌词不会被自动关掉；值为 true 时，自动收放也不会去改
    /// `desktop_lyrics_visible`，免得把用户"手动开着"的意图覆盖掉。
    pub desktop_lyrics_follow_playback: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            follow_app: None,
            // 浏览器和视频播放器是最大的噪声源：放个 B 站视频也会注册 SMTC 会话，
            // 而且 B 站还谎报 PlaybackType=music，只能靠 AUMID 排除。
            exclude_apps: vec![
                "msedge".into(),
                "chrome".into(),
                "firefox".into(),
                "potplayer".into(),
                "vlc".into(),
                "mpc-hc".into(),
            ],
            allow_video: false,
            window_fallback: true,
            window_process: "kugou.exe".into(),
            // 酷狗实测标题格式（2026-11 版，跑马灯滚动）：
            //   " - Into You - 酷狗音乐 Ariana Grande"
            window_regex: r"^\s*-\s*(?P<title>.+?)\s+-\s+酷狗音乐\s*(?P<artist>.*)$".into(),
            lyric_offset_ms: 0,
            per_app_offset: HashMap::new(),
            island_visible: true,
            launch_at_startup: false,
            shortcuts_enabled: false,
            tray_hint_shown: false,
            show_translation: true,
            translation_size: 11.0,
            show_progress: true,
            collapse_on_pause: false,
            desktop_lyrics_visible: false,
            // 默认锁定：桌面歌词的第一诉求是"看"，不是"点"。
            // 解锁是显式操作，用户要拖它的时候自己解。
            desktop_lyrics_locked: true,
            desktop_lyrics_pos: None,
            desktop_lyrics_font_size: 30.0,
            // 默认关：自动弹窗是"惊喜"，不该是默认行为。
            desktop_lyrics_follow_playback: false,
        }
    }
}

static STATE: Mutex<Option<Settings>> = Mutex::new(None);

/// 读当前设置（内存）。没初始化过就返回默认值。
pub fn get() -> Settings {
    STATE
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default()
}

/// 只更新内存（不做副作用、不落盘）
pub fn set_in_memory(s: Settings) {
    if let Ok(mut g) = STATE.lock() {
        *g = Some(s);
    }
}

/// 从 store 读。任何失败都退化成默认值 —— 首次运行文件不存在是正常情况。
pub fn load_from_store<R: Runtime>(app: &AppHandle<R>) -> Settings {
    let loaded = (|| -> Option<Settings> {
        let store = app.store(STORE_FILE).ok()?;
        let value = store.get(STORE_KEY)?;
        serde_json::from_value::<Settings>(value.clone()).ok()
    })();

    match loaded {
        Some(s) => {
            eprintln!("[settings] 已从 {} 载入", STORE_FILE);
            s
        }
        None => {
            eprintln!("[settings] 没有可用的配置文件，使用默认值");
            Settings::default()
        }
    }
}

/// 写回 store。失败只记日志 —— 设置功能不能因为磁盘问题把整个应用带崩。
pub fn save_to_store<R: Runtime>(app: &AppHandle<R>, s: &Settings) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| format!("打开配置存储失败：{e}"))?;
    let value = serde_json::to_value(s).map_err(|e| format!("序列化设置失败：{e}"))?;
    store.set(STORE_KEY, value);
    store.save().map_err(|e| format!("保存设置失败：{e}"))
}

// ============================================================
// 命令
// ============================================================

#[tauri::command]
pub fn get_settings() -> Settings {
    get()
}

/// 写设置：内存 → store → 副作用 → 广播。
///
/// 副作用顺序很重要：先落盘再动手，这样即使副作用抛错，下次启动读到的
/// 也是用户最后一次的意图（而不是"改了但没存"）。
#[tauri::command]
pub async fn set_settings(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    set_in_memory(settings.clone());
    save_to_store(&app, &settings)?;

    // 广播给所有窗口（岛体 / 设置页 / 桌面歌词 / 完整歌词），
    // 用全局 emit 而不是逐个 emit_to —— 窗口会越加越多，
    // 漏掉一个就是"改了没反应"。
    let _ = app.emit("settings:changed", &settings);

    Ok(settings)
}

/// 只改一个字段的工具命令，避免前端为了改个偏移把整个对象回传
#[tauri::command]
pub async fn patch_settings(
    app: AppHandle,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    let s = get();
    let mut base = serde_json::to_value(&s).map_err(|e| e.to_string())?;
    if let (Some(obj), Some(p)) = (base.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            obj.insert(k.clone(), v.clone());
        }
    }
    let merged: Settings = serde_json::from_value(base).map_err(|e| format!("合并设置失败：{e}"))?;
    set_in_memory(merged.clone());
    save_to_store(&app, &merged)?;
    let _ = app.emit("settings:changed", &merged);
    Ok(merged)
}

/// 当前生效的歌词偏移（按应用 > 全局 > 0）
pub fn effective_offset(app_id: &str) -> i32 {
    let s = get();
    if let Some(v) = s.per_app_offset.get(app_id) {
        return *v;
    }
    s.lyric_offset_ms
}

#[tauri::command]
pub fn get_effective_offset(app_id: String) -> i32 {
    effective_offset(&app_id)
}

/// 显示 / 隐藏岛体。托盘、快捷键、设置页都走这一条路径。
pub fn apply_island_visible<R: Runtime>(app: &AppHandle<R>, visible: bool) {
    if let Some(win) = app.get_webview_window("island") {
        if visible {
            let _ = win.show();
        } else {
            let _ = win.hide();
        }
    }
}

/// 同步写：内存 + 落盘 + 广播。
/// 托盘和快捷键这些同步回调里用这个（它们没法 await async 命令）。
pub fn apply<R: Runtime>(app: &AppHandle<R>, s: Settings) {
    set_in_memory(s.clone());
    if let Err(e) = save_to_store(app, &s) {
        eprintln!("[settings] 保存失败：{e}");
    }
    let _ = app.emit("settings:changed", &s);
}

// ============================================================
// 设置窗口
// ============================================================

/// 打开设置窗口（已存在就聚焦，不重复创建）。
///
/// 和岛体分开一个窗口：岛体是透明置顶的异形窗口，塞设置进去会把
/// 宽度自己适应那一套逻辑搅乱，体验也怪。
pub fn open_settings_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return;
    }

    let builder = tauri::WebviewWindowBuilder::new(
        app,
        "settings",
        tauri::WebviewUrl::App("index.html?window=settings".into()),
    )
    .title("Lyric Island · 设置")
    .inner_size(580.0, 720.0)
    .min_inner_size(480.0, 520.0)
    .resizable(true)
    .center()
    .decorations(true)
    .transparent(false)
    .always_on_top(false)
    // 设置页不走透明，但 Tauri 默认给 devtools 是关的，调试时想开就改这里
    .devtools(true);

    if let Err(e) = builder.build() {
        eprintln!("[settings] 打开设置窗口失败：{e}");
    }
}

// ============================================================
// 开机自启
//
// 链路固定为：设置页开关 → 命令 → 插件 + store 双写。
// 不让前端直接调 plugin 的原因：那样 store 和系统状态会分叉，
// 下次启动读到的是"store 说开了、系统里没开"。
// ============================================================

#[tauri::command]
pub fn get_autostart(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// 打开/关闭开机自启。失败要把 store 里的值回滚，不能留下不一致状态。
#[tauri::command]
pub async fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;

    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };

    match result {
        Ok(()) => {
            // 以系统真实状态为准回写，防止插件"返回成功但其实没生效"
            let actual = manager.is_enabled().unwrap_or(enabled);
            let mut s = get();
            s.launch_at_startup = actual;
            apply(&app, s);
            Ok(actual)
        }
        Err(e) => {
            // 回滚：把内存和 store 都改回系统实际状态
            let actual = manager.is_enabled().unwrap_or(false);
            let mut s = get();
            s.launch_at_startup = actual;
            apply(&app, s);
            Err(format!("设置开机自启失败：{e}"))
        }
    }
}
