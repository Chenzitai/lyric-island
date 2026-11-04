//! SMTC（System Media Transport Controls）会话监听 + 兼容层。
//!
//! 走 Windows.Media.Control —— 和 Lyricod / Lyricify 完全同一条路：
//! 只要播放器把媒体信息交给了系统（Spotify、QQ音乐、网易云 UWP、Apple Music、
//! 酷狗、Foobar2000 + 插件……），这里就能读到。
//!
//! 设计取舍：**用 500ms 轮询而不是纯事件订阅**。
//!
//! 事件（MediaPropertiesChanged / TimelinePropertiesChanged / PlaybackInfoChanged）
//! 每个会话都要单独注册 TypedEventHandler，切歌、切播放器、播放器退出时还得逐个
//! 解绑，漏一个就是泄漏或野回调。而轮询只要读 4 个属性，500ms 一次的开销可以忽略，
//! 且天然处理"切歌 / 换播放器 / 会话消失"。
//!
//! 兼容层解决三个真实问题：
//!
//! 1. **酷狗这类播放器有歌名没进度**。TimelineProperties 的 EndTime 恒为 0、
//!    Position 纹丝不动。这种会话要标记 `has_timeline = false`，前端换成
//!    "内部计时器"模式（只在切歌/播放暂停时对表，不每帧重置）。
//! 2. **视频会话会来抢位**。Edge 放个 B 站视频也会注册会话，
//!    靠 MediaProperties.PlaybackType 区分 Music / Video。
//! 3. **多会话同时存在**。不能简单取第一个，要按"正在播放 > 有时间轴 >
//!    信息完整 > 系统当前会话"打分排序。

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

use windows::core::HSTRING;
use windows::Media::MediaPlaybackType;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};

use crate::wininfo;

// ============================================================
// 配置
// ============================================================

/// SMTC 侧需要的配置切片 —— 真正的权威在 `settings.rs`。
///
/// 早期这里自己维护一份 `AppConfig`，加了持久化之后必须收敛到一处，
/// 否则会出现"设置页改了、这里还是旧值"的经典分叉。
pub type AppConfig = crate::settings::Settings;

fn config() -> AppConfig {
    crate::settings::get()
}

/// 正则编译很贵，缓存上一次编译好的（配置没变就不重编）
static REGEX_CACHE: Mutex<Option<(String, Regex)>> = Mutex::new(None);

fn compiled_regex(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }
    if let Ok(guard) = REGEX_CACHE.lock() {
        if let Some((p, re)) = guard.as_ref() {
            if p == pattern {
                return Some(re.clone());
            }
        }
    }
    let re = Regex::new(pattern).ok()?;
    if let Ok(mut guard) = REGEX_CACHE.lock() {
        *guard = Some((pattern.to_string(), re.clone()));
    }
    Some(re)
}

// ============================================================
// 数据结构
// ============================================================

/// 推给前端的当前播放状态
#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct NowPlaying {
    pub active: bool,
    pub session_id: String,
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// data:image/png;base64,... —— 没封面时为空串
    pub artwork: String,
    pub playing: bool,
    pub position_ms: f64,
    pub duration_ms: f64,
    /// "music" | "video" | "image" | "unknown"
    pub playback_type: String,
    /// 播放器是否真的在报进度。false 时前端必须走内部计时器
    pub has_timeline: bool,
    /// 这一帧的数据来源："smtc" | "window"（窗口标题回退）
    pub timeline_source: String,
    /// 窗口回退模式下窗口标题原文，方便诊断面板排查正则
    pub window_title: String,
    /// 采样时刻（epoch ms）。前端用 now - lastUpdated 做插值
    pub last_updated_ms: f64,
}

/// 列给设置页 / 诊断面板用的会话清单
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntry {
    pub session_id: String,
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub playback_type: String,
    pub has_timeline: bool,
    pub position_ms: f64,
    pub duration_ms: f64,
    pub is_current: bool,
}

#[derive(Deserialize, Debug, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ControlAction {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
}

/// 内部用的会话快照
#[derive(Clone, Debug, Default)]
struct Meta {
    app_id: String,
    title: String,
    artist: String,
    album: String,
    playing: bool,
    playback_type: String,
    has_timeline: bool,
    pos: f64,
    dur: f64,
}

/// 上一次推送的内容，用来做去重（避免每 500ms 都刷新一次封面）
static LAST_ARTWORK_KEY: Mutex<String> = Mutex::new(String::new());
static LAST_ARTWORK: Mutex<String> = Mutex::new(String::new());

/// 每个会话的进度推进情况 —— 用来判断"到底有没有时间轴"
struct TimelineWatch {
    last_pos: f64,
    moved_at: Instant,
    ok: bool,
}
static WATCH: Mutex<Vec<(String, TimelineWatch)>> = Mutex::new(Vec::new());

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// 把当前线程初始化成 MTA。
fn init_com() {
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// TimeSpan 是 100ns 单位
fn ts_to_ms(ts: windows::Foundation::TimeSpan) -> f64 {
    ts.Duration as f64 / 10_000.0
}

fn hstr(h: windows::core::Result<HSTRING>) -> String {
    h.map(|s| s.to_string_lossy()).unwrap_or_default()
}

fn playback_type_str(t: MediaPlaybackType) -> &'static str {
    match t {
        MediaPlaybackType::Music => "music",
        MediaPlaybackType::Video => "video",
        MediaPlaybackType::Image => "image",
        _ => "unknown",
    }
}

// ============================================================
// 时间轴检测
// ============================================================

/// 判断某个会话到底有没有时间轴。
///
/// 三级判据：
/// 1. EndTime > 1s —— 播放器自报了总时长，直接信；
/// 2. Position 在推进（两次采样差 > 50ms）—— 有进度但没时长，也算有时间轴；
/// 3. 暂停时位置本来就不动，保持上一次的结论，不要误判成"没时间轴"。
fn update_timeline(app_id: &str, pos: f64, end: f64, playing: bool) -> bool {
    let mut guard = match WATCH.lock() {
        Ok(g) => g,
        Err(_) => return end > 1000.0,
    };
    let now = Instant::now();

    let idx = match guard.iter().position(|(k, _)| k == app_id) {
        Some(i) => i,
        None => {
            guard.push((
                app_id.to_string(),
                TimelineWatch {
                    last_pos: pos,
                    moved_at: now,
                    ok: false,
                },
            ));
            guard.len() - 1
        }
    };

    if end > 1000.0 {
        let w = &mut guard[idx].1;
        w.ok = true;
        w.last_pos = pos;
        w.moved_at = now;
        return true;
    }

    let w = &mut guard[idx].1;
    if !playing {
        return w.ok;
    }

    if (pos - w.last_pos).abs() > 50.0 {
        w.moved_at = now;
        w.ok = true;
    } else if w.moved_at.elapsed() > Duration::from_millis(2500) {
        // 播放中有 2.5 秒纹丝不动 —— 酷骨拖、Foobar 无插件时的典型表现
        w.ok = false;
    }
    w.last_pos = pos;
    w.ok
}

// ============================================================
// 会话读取与打分
// ============================================================

fn read_meta(session: &Session) -> Meta {
    let mut m = Meta {
        app_id: hstr(session.SourceAppUserModelId()),
        playback_type: "unknown".into(),
        ..Default::default()
    };

    if let Ok(props) = session.TryGetMediaPropertiesAsync().and_then(|op| op.get()) {
        m.title = hstr(props.Title());
        m.artist = hstr(props.Artist());
        m.album = hstr(props.AlbumTitle());
        if let Ok(pt) = props.PlaybackType() {
            // PlaybackType 返回的是 IReference<MediaPlaybackType>，要再取一次值
            if let Ok(value) = pt.Value() {
                m.playback_type = playback_type_str(value).to_string();
            }
        }
    }

    if let Ok(info) = session.GetPlaybackInfo() {
        if let Ok(status) = info.PlaybackStatus() {
            m.playing = status == PlaybackStatus::Playing;
        }
    }

    if let Ok(timeline) = session.GetTimelineProperties() {
        let start = timeline.StartTime().map(ts_to_ms).unwrap_or(0.0);
        let pos = timeline.Position().map(ts_to_ms).unwrap_or(0.0);
        let end = timeline.EndTime().map(ts_to_ms).unwrap_or(0.0);
        m.pos = (pos - start).max(0.0);
        m.dur = (end - start).max(0.0);
        m.has_timeline = update_timeline(&m.app_id, m.pos, m.dur, m.playing);
    } else {
        m.has_timeline = false;
    }

    m
}

/// 给候选会话打分。返回 None 表示"这个会话不该被跟随"。
fn score(m: &Meta, is_current: bool, cfg: &AppConfig) -> Option<i32> {
    let lower = m.app_id.to_lowercase();

    if cfg
        .exclude_apps
        .iter()
        .any(|e| !e.is_empty() && lower.contains(&e.to_lowercase()))
    {
        return None;
    }

    // 视频会话默认直接踢掉 —— 否则放个视频就跳出一个空壳岛
    if m.playback_type == "video" && !cfg.allow_video {
        return None;
    }

    // 连歌名都没有的会话没有任何展示价值
    if m.title.is_empty() {
        return None;
    }

    let mut s = 0;
    if m.playing {
        s += 100;
    }
    if m.has_timeline {
        s += 30;
    }
    if !m.artist.is_empty() {
        s += 10;
    }
    if is_current {
        s += 50;
    }

    // 启发式降权：网页视频的标题往往又长又没有歌手，还会带书名号。
    // 实测 B 站客户端会谎报 PlaybackType=music，标题长这样：
    //   "【koPanda ASMR｜3Dio深度采耳助眠无人声】枕边清梦入无声，半榻幽光照影清"
    // 惩罚要足够重（-60），否则它和酷狗打平（B站靠"系统当前"多出的 50 分），
    // 谁先被遍历到谁赢，结果随机。
    let chars = m.title.chars().count();
    if m.artist.is_empty() && chars > 30 {
        s -= 60;
    }

    Some(s)
}

struct Selected {
    session: Session,
    meta: Meta,
}

/// 从所有会话里挑一个最该跟随的。
fn pick_best(manager: &SessionManager, cfg: &AppConfig) -> Option<Selected> {
    let current_id = manager
        .GetCurrentSession()
        .map(|s| hstr(s.SourceAppUserModelId()))
        .unwrap_or_default();

    let sessions = manager.GetSessions().ok()?;

    let mut best: Option<(Session, Meta, i32)> = None;

    for s in sessions {
        let m = read_meta(&s);

        // 手动指定了就只看那一个，别的一律跳过
        if let Some(target) = &cfg.follow_app {
            if !target.is_empty() && &m.app_id != target {
                continue;
            }
        }

        let is_current = m.app_id == current_id;
        let Some(sc) = score(&m, is_current, cfg) else {
            continue;
        };

        if best.as_ref().map_or(true, |b| sc > b.2) {
            best = Some((s, m, sc));
        }
    }

    best.map(|(session, meta, _)| Selected { session, meta })
}

/// 窗口回退通道。返回 (歌名, 歌手, 原始标题)。
///
/// 只在 SMTC 完全给不出音乐会话时用。
///
/// **坑**：酷狗的窗口标题是跑马灯滚动字幕，同一首歌里标题会不停变，比如
///   " - Into You - 酷狗音乐 Ariana Grande"
///   "u - 酷狗音乐 Ariana Grande - Into Yo"
/// 正则不可能每次都匹配上。所以这里记住"上一次成功解析的结果"，
/// 解析失败就沿用，最多保持 30 秒 —— 否则歌词会在滚动到边界时闪回"暂无歌词"。
fn window_fallback(cfg: &AppConfig) -> Option<(String, String, String)> {
    if !cfg.window_fallback {
        return None;
    }

    let parsed = parse_window_title(cfg);

    let mut last = match LAST_WINDOW.lock() {
        Ok(g) => g,
        Err(_) => return parsed.map(|(t, a, raw)| (t, a, raw)),
    };

    match parsed {
        Some((t, a, raw)) => {
            *last = Some((t.clone(), a.clone(), Instant::now()));
            Some((t, a, raw))
        }
        None => match last.as_ref() {
            // 30 秒内上一次的解析结果还算数（酷狗切歌时也会短暂解析失败）
            Some((t, a, at)) if at.elapsed() < Duration::from_secs(30) => {
                Some((t.clone(), a.clone(), String::new()))
            }
            _ => None,
        },
    }
}

static LAST_WINDOW: Mutex<Option<(String, String, Instant)>> = Mutex::new(None);

fn parse_window_title(cfg: &AppConfig) -> Option<(String, String, String)> {
    let re = compiled_regex(&cfg.window_regex)?;
    let want = cfg.window_process.to_lowercase();

    for w in wininfo::list_visible_windows() {
        if !want.is_empty() && w.process != want {
            continue;
        }
        if want.is_empty() && w.process.is_empty() {
            continue;
        }
        if let Some(caps) = re.captures(&w.title) {
            let title = caps
                .name("title")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let artist = caps
                .name("artist")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            if !title.is_empty() {
                return Some((title, artist, w.title));
            }
        }
    }
    None
}

// ============================================================
// 后台监听线程
// ============================================================

/// 启动后台监听线程。失败不 panic —— 前端会显示"未连接媒体会话"。
pub fn spawn(app: AppHandle) {
    std::thread::Builder::new()
        .name("smtc-watcher".into())
        .spawn(move || {
            init_com();

            let mut manager: Option<SessionManager> = None;
            let mut last_payload = String::new();
            let mut last_pos = 0.0_f64;
            let mut last_art = String::new();
            let mut last_emit = Instant::now() - Duration::from_secs(10);
            let mut tick: u64 = 0;
            eprintln!("[smtc] watcher 线程已启动");

            loop {
                if manager.is_none() {
                    match SessionManager::RequestAsync().and_then(|op| op.get()) {
                        Ok(m) => {
                            eprintln!("[smtc] SessionManager 就绪");
                            manager = Some(m);
                        }
                        Err(e) => {
                            eprintln!("[smtc] 获取 SessionManager 失败：{e}");
                            std::thread::sleep(Duration::from_secs(2));
                            continue;
                        }
                    }
                }

                let cfg = config();
                let payload = match &manager {
                    Some(m) => read_state(m, &cfg),
                    None => NowPlaying::default(),
                };

                // 会话级时钟：喂一帧，顺便看这一帧是不是用户拖了进度条
                if let Some(seek_to) = crate::clock::tick(&payload) {
                    let _ = app.emit("smtc:seek", serde_json::json!({ "positionMs": seek_to }));
                }

                // 跟随播放自动收放桌面歌词。放在时钟和托盘刷新之后：
                // 那两步只读状态，这一步会动窗口，越靠后越安全。
                crate::follow::tick(&app, &payload);

                // 顺手刷新托盘要用的快照（每 4 帧 = 2 秒一次，别每帧都枚举）
                if tick % 4 == 0 {
                    if let Some(m) = &manager {
                        store_snapshot(m, &payload);
                        crate::tray::refresh(&app);
                    }
                }
                tick = tick.wrapping_add(1);

                // 去重 key 里必须挖掉两个"每帧都在变"的字段，否则去重形同虚设
                let mut cmp = payload.clone();
                cmp.last_updated_ms = 0.0;
                cmp.position_ms = 0.0;
                cmp.artwork = String::new();
                let json = serde_json::to_string(&cmp).unwrap_or_default();

                let drift = (payload.position_ms - last_pos).abs();
                let content_changed = json != last_payload;
                // 心跳：内容完全没变也要每 2.5 秒重推一次
                let heartbeat = last_emit.elapsed() > Duration::from_millis(2500);

                if content_changed || drift > 900.0 || heartbeat {
                    let mut out = payload.clone();
                    if out.artwork == last_art {
                        out.artwork.clear();
                    } else {
                        last_art = out.artwork.clone();
                    }

                    last_payload = json;
                    last_pos = payload.position_ms;
                    last_emit = Instant::now();
                    let _ = app.emit("smtc:update", &out);
                }

                std::thread::sleep(Duration::from_millis(500));
            }
        })
        .ok();
}

/// 读一帧完整状态：SMTC 优先，窗口标题兜底。
fn read_state(manager: &SessionManager, cfg: &AppConfig) -> NowPlaying {
    if let Some(sel) = pick_best(manager, cfg) {
        let mut np = NowPlaying {
            active: true,
            session_id: sel.meta.app_id.clone(),
            app_id: sel.meta.app_id.clone(),
            title: sel.meta.title.clone(),
            artist: sel.meta.artist.clone(),
            album: sel.meta.album.clone(),
            playing: sel.meta.playing,
            position_ms: sel.meta.pos,
            duration_ms: sel.meta.dur,
            playback_type: sel.meta.playback_type.clone(),
            has_timeline: sel.meta.has_timeline,
            timeline_source: "smtc".into(),
            ..Default::default()
        };
        np.artwork = artwork_for(&sel.session, &np.title, &np.artist, &np.album);
        np.last_updated_ms = now_ms();
        return np;
    }

    // SMTC 里没有可用的音乐会话 —— 试试窗口标题
    if let Some((title, artist, raw)) = window_fallback(cfg) {
        return NowPlaying {
            active: true,
            session_id: format!("window:{}", cfg.window_process),
            app_id: format!("window:{}", cfg.window_process),
            title,
            artist,
            album: String::new(),
            artwork: String::new(),
            // 窗口标题看不出播没播，假定在播 —— 否则岛会一直灰着
            playing: true,
            position_ms: 0.0,
            duration_ms: 0.0,
            playback_type: "music".into(),
            has_timeline: false,
            timeline_source: "window".into(),
            window_title: raw,
            last_updated_ms: now_ms(),
        };
    }

    NowPlaying::default()
}

/// 封面按 key 缓存，同一首歌不重复传 base64
fn artwork_for(session: &Session, title: &str, artist: &str, album: &str) -> String {
    let key = format!("{title}|{artist}|{album}");
    let cached_key = LAST_ARTWORK_KEY.lock().map(|g| g.clone()).unwrap_or_default();

    if key != cached_key {
        let art = session
            .TryGetMediaPropertiesAsync()
            .and_then(|op| op.get())
            .ok()
            .and_then(|p| p.Thumbnail().ok())
            .and_then(|t| read_thumbnail(t).ok())
            .unwrap_or_default();
        if let Ok(mut g) = LAST_ARTWORK_KEY.lock() {
            *g = key;
        }
        if let Ok(mut g) = LAST_ARTWORK.lock() {
            *g = art;
        }
    }

    LAST_ARTWORK.lock().map(|g| g.clone()).unwrap_or_default()
}

/// 缩略图 → base64 data URL
fn read_thumbnail(ref_img: IRandomAccessStreamReference) -> windows::core::Result<String> {
    let stream = ref_img.OpenReadAsync()?.get()?;
    let size = stream.Size()? as u32;
    if size == 0 || size > 8 * 1024 * 1024 {
        return Ok(String::new());
    }

    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.get()?;

    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf)?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
    Ok(format!("data:image/png;base64,{b64}"))
}

// ============================================================
// 给托盘 / 设置窗口用的状态缓存
//
// 托盘菜单要显示"当前跟随：XXX"和会话列表，但菜单是同步构建的，
// 不能每次都去 RequestAsync 拿 manager（那要进 COM 且要 500ms）。
// 所以监听线程顺手把最近一帧状态和会话快照存下来，托盘读缓存即可。
// ============================================================

static LAST_NP: Mutex<Option<NowPlaying>> = Mutex::new(None);
static LAST_SESSIONS: Mutex<Vec<SessionEntry>> = Mutex::new(Vec::new());

pub fn last_now_playing() -> NowPlaying {
    let mut np = LAST_NP
        .lock()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_default();

    // 快照是 2.5 秒前存的，直接把它交给前端就等于"进度倒退 2.5 秒"。
    // 用时钟外推过的实时进度覆盖它 —— 这是"刷新后不从 0 开始"的关键。
    if np.active {
        let p = crate::clock::progress();
        if !p.app_id.is_empty() && p.app_id == np.app_id {
            np.position_ms = p.position_ms;
            np.playing = p.playing;
            np.has_timeline = p.has_timeline;
            if p.duration_ms > 0.0 {
                np.duration_ms = p.duration_ms;
            }
        }
    }

    np
}

pub fn last_sessions() -> Vec<SessionEntry> {
    LAST_SESSIONS.lock().ok().map(|g| g.clone()).unwrap_or_default()
}

fn store_snapshot(manager: &SessionManager, np: &NowPlaying) {
    if let Ok(mut g) = LAST_NP.lock() {
        *g = Some(np.clone());
    }

    let current_id = manager
        .GetCurrentSession()
        .map(|s| hstr(s.SourceAppUserModelId()))
        .unwrap_or_default();

    let Ok(sessions) = manager.GetSessions() else {
        return;
    };

    let mut out = Vec::new();
    for s in sessions {
        let m = read_meta(&s);
        out.push(SessionEntry {
            is_current: m.app_id == current_id,
            session_id: m.app_id.clone(),
            app_id: m.app_id,
            title: m.title,
            artist: m.artist,
            album: m.album,
            playing: m.playing,
            playback_type: m.playback_type,
            has_timeline: m.has_timeline,
            position_ms: m.pos,
            duration_ms: m.dur,
        });
    }
    if let Ok(mut g) = LAST_SESSIONS.lock() {
        *g = out;
    }
}

// ============================================================
// Tauri commands
// ============================================================

/// 诊断面板用：列出所有会话的详细状态
#[tauri::command]
pub fn list_sessions() -> Result<Vec<SessionEntry>, String> {
    init_com();

    let manager = SessionManager::RequestAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("无法获取媒体会话管理器：{e}"))?;

    let current_id = manager
        .GetCurrentSession()
        .map(|s| hstr(s.SourceAppUserModelId()))
        .unwrap_or_default();

    let sessions = manager
        .GetSessions()
        .map_err(|e| format!("枚举会话失败：{e}"))?;

    let mut out = Vec::new();
    for s in sessions {
        let m = read_meta(&s);
        out.push(SessionEntry {
            is_current: m.app_id == current_id,
            session_id: m.app_id.clone(),
            app_id: m.app_id,
            title: m.title,
            artist: m.artist,
            album: m.album,
            playing: m.playing,
            playback_type: m.playback_type,
            has_timeline: m.has_timeline,
            position_ms: m.pos,
            duration_ms: m.dur,
        });
    }
    Ok(out)
}

/// 诊断面板用：把所有可见窗口列出来，方便确定酷狗的进程名和标题格式
#[tauri::command]
pub fn list_windows() -> Vec<wininfo::WindowInfo> {
    wininfo::list_visible_windows()
}

/// 主动拉一次当前状态
#[tauri::command]
pub fn now_playing() -> Result<NowPlaying, String> {
    init_com();

    let manager = SessionManager::RequestAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("无法获取媒体会话管理器：{e}"))?;

    Ok(read_state(&manager, &config()))
}

#[tauri::command]
pub fn get_config() -> AppConfig {
    config()
}

/// 兼容老前端的入口：整包覆盖设置（会落盘 + 广播）
#[tauri::command]
pub async fn set_config(
    app: tauri::AppHandle,
    cfg: AppConfig,
) -> Result<AppConfig, String> {
    crate::settings::set_settings(app, cfg).await
}

/// 播放控制（悬停迷你播放器 + 托盘菜单用）
#[tauri::command]
pub fn send_control(action: ControlAction) -> Result<(), String> {
    init_com();

    let manager = SessionManager::RequestAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("无法获取媒体会话管理器：{e}"))?;

    // 用和监听线程同一套选择逻辑，避免"控制的是一个会话、
    // 显示的是另一个会话"这种错位
    let cfg = config();
    let session: Session = match pick_best(&manager, &cfg) {
        Some(s) => s.session,
        None => manager
            .GetCurrentSession()
            .map_err(|e| format!("没有活动的媒体会话：{e}"))?,
    };

    let r = match action {
        ControlAction::Play => session.TryPlayAsync().and_then(|op| op.get()),
        ControlAction::Pause => session.TryPauseAsync().and_then(|op| op.get()),
        ControlAction::Toggle => session.TryTogglePlayPauseAsync().and_then(|op| op.get()),
        ControlAction::Next => session.TrySkipNextAsync().and_then(|op| op.get()),
        ControlAction::Previous => session.TrySkipPreviousAsync().and_then(|op| op.get()),
    };

    r.map(|_| ()).map_err(|e| format!("控制指令失败：{e}"))
}

/// 进度跳转（点击 / 拖动进度条）。
///
/// SMTC 的 `TimeSpan` 单位是 100ns，所以要乘 10000。
/// 播放器不支持定位时 `TryChangePlaybackPositionAsync` 会返回 false 而不是报错，
/// 必须把这个 false 传回去 —— 前端要靠它提示"当前播放器不支持进度跳转"。
#[tauri::command]
pub fn seek_to(position_ms: f64) -> Result<(), String> {
    if !position_ms.is_finite() || position_ms < 0.0 {
        return Err("无效的跳转位置".into());
    }

    init_com();

    let manager = SessionManager::RequestAsync()
        .and_then(|op| op.get())
        .map_err(|e| format!("无法获取媒体会话管理器：{e}"))?;

    let cfg = config();
    let session: Session = match pick_best(&manager, &cfg) {
        Some(s) => s.session,
        None => manager
            .GetCurrentSession()
            .map_err(|e| format!("没有活动的媒体会话：{e}"))?,
    };

    // 参数直接是 i64 的 100ns ticks，不是 TimeSpan 结构体
    let ticks = (position_ms * 10_000.0) as i64;

    match session.TryChangePlaybackPositionAsync(ticks) {
        Ok(op) => match op.get() {
            Ok(true) => {
                // 立刻把时钟锚点挪过去，否则下一帧会把这跳再判成一次 seek
                crate::clock::force_resync(position_ms);
                Ok(())
            }
            Ok(false) => Err("当前播放器不支持进度跳转".into()),
            Err(e) => Err(format!("进度跳转失败：{e}")),
        },
        Err(e) => Err(format!("进度跳转失败：{e}")),
    }
}
