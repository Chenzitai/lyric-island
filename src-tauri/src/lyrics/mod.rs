//! 歌词获取 —— 多源、严格匹配、手动绑定。
//!
//! 结构：
//!
//! | 文件 | 职责 |
//! | --- | --- |
//! | `mod.rs` | trait、错误模型、多源调度、命令 |
//! | `matcher.rs` | 标题归一化 / 艺术家拆分 / 时长闸门 / 打分 |
//! | `netease.rs` | 网易云（原文 + 翻译 + 音译 + 逐字） |
//! | `qq.rs` | QQ 音乐（原文 + 翻译） |
//! | `bindings.rs` | 用户手动选词的绑定记忆 |
//!
//! 三个设计原则：
//!
//! 1. **Rust 只负责"拿到文本 + 判断可不可信"，解析全在前端 JS**。
//!    LRC 时间戳格式太杂，JS 正则比 Rust 好写，解析结果也直接是渲染层要的结构。
//!
//! 2. **失败必须分类**。用户真正需要区分的是"源没有" / "网络出问题" /
//!    "拿回来但解析不了"，笼统的"获取失败"没法排查。
//!
//! 3. **宁可不显示，不能显示错的**。时长差 > 3 秒的候选直接丢弃，
//!    标题完全不像的直接判死。这一条比"多命中几首"重要得多。
//!
//! 全部走 `reqwest::blocking`，整个流程跑在 `spawn_blocking` 里 ——
//! provider trait 就不用 async-trait，加新源也不用考虑生命周期。

pub mod bindings;
mod matcher;
mod netease;
mod qq;

use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use matcher::Candidate;

// ============================================================
// 错误模型
// ============================================================

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LyricsErrorKind {
    /// 所有源里都没有这首歌
    NotFound,
    /// 网络层失败：超时 / DNS / 连接被重置
    NetworkError,
    /// 拿回内容了，但不是能解析的歌词
    ParseError,
    /// 429
    RateLimited,
    /// 本地参数问题（歌名为空等），压根没发请求
    BadRequest,
    /// 所有 provider 都失败了（多源汇总）
    AllFailed,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LyricsFailure {
    pub kind: LyricsErrorKind,
    pub message: String,
    /// 给诊断面板看的技术细节
    pub detail: String,
    pub source: String,
    /// 所有源的失败明细，多源时用
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub attempts: Vec<String>,
}

impl LyricsFailure {
    fn new(kind: LyricsErrorKind, source: &str, message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            detail: detail.into(),
            source: source.into(),
            attempts: Vec::new(),
        }
    }
}

// ============================================================
// 返回结构
// ============================================================

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LyricsPayload {
    /// 原文，带时间轴
    pub synced: String,
    pub plain: String,
    /// 翻译（可能为空）
    pub translated: String,
    /// 音译 / 罗马音（可能为空）
    pub romanized: String,
    /// 逐字原文（网易云 yrc，没有就是空）
    pub word_synced: String,
    pub instrumental: bool,
    pub word_sync: bool,
    /// 命中的音源标识，如 "netease:1234"
    pub source: String,
    pub matched_title: String,
    pub matched_artist: String,
    pub duration: f64,
    pub line_count: usize,
    /// 是不是用户手动绑定的结果
    pub from_binding: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct LyricsResponse {
    pub ok: bool,
    pub payload: Option<LyricsPayload>,
    pub failure: Option<LyricsFailure>,
}

#[derive(Serialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct LyricsStatusEntry {
    pub track: String,
    pub source: String,
    pub http_status: u16,
    pub ok: bool,
    pub kind: String,
    pub line_count: usize,
    pub detail: String,
    pub elapsed_ms: u64,
    pub at_ms: f64,
    /// 本次请求试过的所有源及结果，诊断面板直接摊开
    pub attempts: Vec<String>,
}

static LAST_STATUS: Mutex<Option<LyricsStatusEntry>> = Mutex::new(None);

pub fn last_status() -> Option<LyricsStatusEntry> {
    LAST_STATUS.lock().ok().and_then(|g| g.clone())
}

fn record_status(entry: LyricsStatusEntry) {
    if let Ok(mut g) = LAST_STATUS.lock() {
        *g = Some(entry);
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

// ============================================================
// Provider 抽象
// ============================================================

#[derive(Clone, Debug)]
pub struct LyricsQuery {
    pub title: String,
    pub artist: String,
    /// 专辑名。当前各源的搜索接口都不支持按专辑过滤，
    /// 留在查询结构里是为了以后接"专辑内匹配"时不必改签名。
    #[allow(dead_code)]
    pub album: Option<String>,
    pub duration: Option<f64>,
}

pub trait LyricsProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// 越大越先试。网易云 30 / QQ 20 / LRCLIB 10。
    fn priority(&self) -> i32;
    /// 搜索候选（不带歌词正文，快）
    fn search(&self, q: &LyricsQuery) -> Result<Vec<Candidate>, LyricsFailure>;
    /// 按候选取歌词
    fn fetch(&self, cand: &Candidate) -> Result<(LyricsPayload, u16), (LyricsFailure, u16)>;
}

// ============================================================
// LRCLIB
// ============================================================

const LRCLIB_BASE: &str = "https://lrclib.net/api";
const LRCLIB_NAME: &str = "lrclib";

#[derive(Deserialize, Debug)]
struct LrclibItem {
    id: i64,
    #[serde(rename = "trackName", default)]
    track_name: String,
    #[serde(rename = "artistName", default)]
    artist_name: String,
    #[serde(rename = "albumName", default)]
    album_name: String,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    instrumental: bool,
    #[serde(rename = "plainLyrics", default)]
    plain_lyrics: Option<String>,
    #[serde(rename = "syncedLyrics", default)]
    synced_lyrics: Option<String>,
    #[serde(rename = "hasWordSync", default)]
    has_word_sync: bool,
}

pub struct LrclibProvider {
    http: reqwest::blocking::Client,
}

impl LrclibProvider {
    pub fn new() -> Result<Self, String> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(6))
            .user_agent("LyricIsland/0.3 (https://github.com/)")
            .build()
            .map_err(|e| format!("创建 LRCLIB 客户端失败：{e}"))?;
        Ok(Self { http })
    }

    fn net_err(&self, e: reqwest::Error, step: &str) -> LyricsFailure {
        let msg = if e.is_timeout() {
            "LRCLIB 请求超时"
        } else if e.is_connect() {
            "连不上 LRCLIB"
        } else {
            "LRCLIB 网络请求失败"
        };
        LyricsFailure::new(LyricsErrorKind::NetworkError, LRCLIB_NAME, msg, format!("{step}: {e}"))
    }
}

impl LyricsProvider for LrclibProvider {
    fn name(&self) -> &'static str {
        LRCLIB_NAME
    }

    fn priority(&self) -> i32 {
        10
    }

    fn search(&self, q: &LyricsQuery) -> Result<Vec<Candidate>, LyricsFailure> {
        let url = format!("{LRCLIB_BASE}/search");
        let resp = self
            .http
            .get(&url)
            .query(&[("track_name", q.title.as_str()), ("artist_name", q.artist.as_str())])
            .send()
            .map_err(|e| self.net_err(e, "/search"))?;

        let code = resp.status().as_u16();
        if code == 429 {
            return Err(LyricsFailure::new(
                LyricsErrorKind::RateLimited,
                LRCLIB_NAME,
                "LRCLIB 限流了，稍后再试",
                "/search: HTTP 429",
            ));
        }
        if !resp.status().is_success() {
            return Err(LyricsFailure::new(
                LyricsErrorKind::NetworkError,
                LRCLIB_NAME,
                "LRCLIB 服务端出错",
                format!("/search: HTTP {code}"),
            ));
        }

        let items = resp.json::<Vec<LrclibItem>>().map_err(|e| {
            LyricsFailure::new(
                LyricsErrorKind::ParseError,
                LRCLIB_NAME,
                "LRCLIB 返回的内容不是预期格式",
                format!("/search 反序列化失败：{e}"),
            )
        })?;

        Ok(items
            .into_iter()
            .map(|i| Candidate {
                source: LRCLIB_NAME.into(),
                source_id: i.id.to_string(),
                title: i.track_name,
                artist: i.artist_name,
                album: i.album_name,
                duration: i.duration,
                has_synced: i.synced_lyrics.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false),
                has_word_sync: i.has_word_sync,
                has_translation: false,
                preview: i
                    .plain_lyrics
                    .as_ref()
                    .map(|p| p.lines().take(2).collect::<Vec<_>>().join(" / "))
                    .unwrap_or_default(),
                score: 0,
            })
            .collect())
    }

    fn fetch(&self, cand: &Candidate) -> Result<(LyricsPayload, u16), (LyricsFailure, u16)> {
        let url = format!("{LRCLIB_BASE}/get/{}", cand.source_id);
        let resp = self.http.get(&url).send().map_err(|e| (self.net_err(e, "/get"), 0))?;

        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err((
                LyricsFailure::new(
                    LyricsErrorKind::NotFound,
                    LRCLIB_NAME,
                    "LRCLIB 取歌词失败",
                    format!("/get/{}: HTTP {code}", cand.source_id),
                ),
                code,
            ));
        }

        let item = resp.json::<LrclibItem>().map_err(|e| {
            (
                LyricsFailure::new(
                    LyricsErrorKind::ParseError,
                    LRCLIB_NAME,
                    "LRCLIB 歌词不是预期格式",
                    format!("/get 反序列化失败：{e}"),
                ),
                code,
            )
        })?;

        let synced = item.synced_lyrics.unwrap_or_default();
        let plain = item.plain_lyrics.unwrap_or_default();
        if synced.trim().is_empty() && plain.trim().is_empty() {
            return Err((
                LyricsFailure::new(
                    LyricsErrorKind::NotFound,
                    LRCLIB_NAME,
                    "LRCLIB 只有空歌词",
                    format!("/get/{}: 两个字段都为空", cand.source_id),
                ),
                code,
            ));
        }

        let line_count = synced.lines().filter(|l| l.trim_start().starts_with('[')).count();

        Ok((
            LyricsPayload {
                synced,
                plain,
                translated: String::new(),
                romanized: String::new(),
                word_synced: String::new(),
                instrumental: item.instrumental,
                word_sync: item.has_word_sync,
                source: format!("lrclib:{}", item.id),
                matched_title: item.track_name,
                matched_artist: item.artist_name,
                duration: item.duration,
                line_count,
                from_binding: false,
            },
            code,
        ))
    }
}

// ============================================================
// 多源调度
// ============================================================

fn providers() -> Vec<Box<dyn LyricsProvider>> {
    let mut list: Vec<Box<dyn LyricsProvider>> = Vec::new();
    match netease::NeteaseProvider::new() {
        Ok(p) => list.push(Box::new(p)),
        Err(e) => eprintln!("[lyrics] 网易云初始化失败：{e}"),
    }
    match qq::QqProvider::new() {
        Ok(p) => list.push(Box::new(p)),
        Err(e) => eprintln!("[lyrics] QQ 音乐初始化失败：{e}"),
    }
    match LrclibProvider::new() {
        Ok(p) => list.push(Box::new(p)),
        Err(e) => eprintln!("[lyrics] LRCLIB 初始化失败：{e}"),
    }
    list.sort_by(|a, b| b.priority().cmp(&a.priority()));
    list
}

fn provider_by_name(name: &str) -> Option<Box<dyn LyricsProvider>> {
    providers().into_iter().find(|p| p.name() == name)
}

/// 收集所有源的候选，打分、过滤、排序。返回 (候选, 每个源的失败明细)。
fn collect_candidates(q: &LyricsQuery) -> (Vec<Candidate>, Vec<String>) {
    let q_artists = matcher::split_artists(&q.artist);
    let mut all: Vec<Candidate> = Vec::new();
    let mut attempts: Vec<String> = Vec::new();

    for p in providers() {
        match p.search(q) {
            Ok(list) => {
                let raw = list.len();
                let mut kept = 0;
                for mut c in list {
                    // 硬闸门：时长差 > 3 秒直接丢
                    if !matcher::duration_ok(c.duration, q.duration) {
                        continue;
                    }
                    let score = matcher::score_candidate(&c, &q.title, &q_artists, q.duration);
                    if score <= -999 {
                        continue;
                    }
                    c.score = score;
                    all.push(c);
                    kept += 1;
                }
                attempts.push(format!("{}: 搜到 {raw} 条，保留 {kept}", p.name()));
            }
            Err(e) => {
                attempts.push(format!("{}: {}（{}）", p.name(), e.message, e.detail));
            }
        }
    }

    all.sort_by(|a, b| b.score.cmp(&a.score));
    (all, attempts)
}

/// 给前 n 个候选补歌词预览（选词窗口用）。失败的候选保留但不带预览。
fn enrich_previews(list: &mut [Candidate], n: usize) {
    for c in list.iter_mut().take(n) {
        if !c.preview.trim().is_empty() {
            continue;
        }
        if let Some(p) = provider_by_name(&c.source) {
            if let Ok((payload, _)) = p.fetch(c) {
                c.has_synced = payload.line_count > 0;
                c.has_word_sync = payload.word_sync;
                c.has_translation = !payload.translated.trim().is_empty();
                let src = if payload.translated.trim().is_empty() {
                    payload.synced.clone()
                } else {
                    payload.synced.clone()
                };
                c.preview = src
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| {
                        // 去掉时间戳再当预览
                        match l.rfind(']') {
                            Some(i) => l[i + 1..].trim().to_string(),
                            None => l.trim().to_string(),
                        }
                    })
                    .filter(|l| !l.is_empty())
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" / ");
            }
        }
    }
}

/// 从候选里取歌词：按分数从高到低试，第一个成功就返回。
fn fetch_from_candidates(cands: &[Candidate], max_try: usize) -> Result<(LyricsPayload, u16), (LyricsFailure, Vec<String>)> {
    let mut attempts: Vec<String> = Vec::new();
    let mut last: Option<(LyricsFailure, u16)> = None;

    for c in cands.iter().take(max_try) {
        let Some(p) = provider_by_name(&c.source) else {
            attempts.push(format!("{}: 源不可用", c.source));
            continue;
        };
        match p.fetch(c) {
            Ok(ok) => return Ok(ok),
            Err((f, code)) => {
                attempts.push(format!("{}:{} {}（{}）", c.source, c.source_id, f.message, f.detail));
                if last.is_none() {
                    last = Some((f, code));
                }
            }
        }
    }

    let (f, _code) = last.unwrap_or_else(|| {
        (
            LyricsFailure::new(
                LyricsErrorKind::NotFound,
                "lyrics",
                "所有歌词源都没有这首歌",
                "候选列表为空",
            ),
            0,
        )
    });
    Err((f, attempts))
}

// ============================================================
// 命令
// ============================================================

fn kind_str(k: LyricsErrorKind) -> &'static str {
    match k {
        LyricsErrorKind::NotFound => "notFound",
        LyricsErrorKind::NetworkError => "networkError",
        LyricsErrorKind::ParseError => "parseError",
        LyricsErrorKind::RateLimited => "rateLimited",
        LyricsErrorKind::BadRequest => "badRequest",
        LyricsErrorKind::AllFailed => "allFailed",
    }
}

fn respond_ok(payload: LyricsPayload, http_code: u16, track: String, elapsed: u64, attempts: Vec<String>) -> LyricsResponse {
    record_status(LyricsStatusEntry {
        track,
        source: payload.source.clone(),
        http_status: http_code,
        ok: true,
        kind: "ok".into(),
        line_count: payload.line_count,
        detail: format!("命中 {} 行", payload.line_count),
        elapsed_ms: elapsed,
        at_ms: now_ms(),
        attempts,
    });
    LyricsResponse {
        ok: true,
        payload: Some(payload),
        failure: None,
    }
}

fn respond_err(mut failure: LyricsFailure, http_code: u16, track: String, elapsed: u64, attempts: Vec<String>) -> LyricsResponse {
    failure.attempts = attempts.clone();
    record_status(LyricsStatusEntry {
        track,
        source: failure.source.clone(),
        http_status: http_code,
        ok: false,
        kind: kind_str(failure.kind).into(),
        line_count: 0,
        detail: failure.detail.clone(),
        elapsed_ms: elapsed,
        at_ms: now_ms(),
        attempts,
    });
    LyricsResponse {
        ok: false,
        payload: None,
        failure: Some(failure),
    }
}

#[tauri::command]
pub async fn fetch_lyrics(
    app: tauri::AppHandle,
    title: String,
    artist: String,
    album: Option<String>,
    duration: Option<f64>,
) -> LyricsResponse {
    let started = std::time::Instant::now();

    if title.trim().is_empty() {
        return respond_err(
            LyricsFailure::new(LyricsErrorKind::BadRequest, "local", "歌曲名为空，无法搜索歌词", "title 是空串"),
            0,
            String::new(),
            0,
            Vec::new(),
        );
    }

    let track = if artist.trim().is_empty() {
        title.clone()
    } else {
        format!("{title} - {artist}")
    };

    // ---- 0) 先看有没有用户手动绑定的结果 ----
    let key = matcher::track_key(&title, &artist);
    if let Some(b) = bindings::get(&app, &key) {
        let cand = Candidate {
            source: b.source.clone(),
            source_id: b.source_id.clone(),
            title: b.title.clone(),
            artist: b.artist.clone(),
            album: String::new(),
            duration: b.duration,
            has_synced: true,
            has_word_sync: false,
            has_translation: false,
            preview: String::new(),
            score: 999,
        };
        let result = tauri::async_runtime::spawn_blocking(move || {
            match provider_by_name(&cand.source) {
                Some(p) => p.fetch(&cand).map(|(mut pl, code)| {
                    pl.from_binding = true;
                    (pl, code)
                }),
                None => Err((
                    LyricsFailure::new(LyricsErrorKind::AllFailed, "binding", "绑定的歌词源已不可用", &cand.source),
                    0,
                )),
            }
        })
        .await;

        if let Ok(Ok((payload, code))) = result {
            return respond_ok(
                payload,
                code,
                track,
                started.elapsed().as_millis() as u64,
                vec![format!("使用手动绑定：{}", b.source)],
            );
        }
        // 绑定失效就把脏数据清掉，然后正常走自动匹配
        let _ = bindings::remove(&app, &key);
    }

    // ---- 1) 多源搜索 + 打分 ----
    let query = LyricsQuery {
        title: title.clone(),
        artist: artist.clone(),
        album,
        duration,
    };

    let searched = tauri::async_runtime::spawn_blocking({
        let query = query.clone();
        move || {
            let (cands, attempts) = collect_candidates(&query);
            let fetched = fetch_from_candidates(&cands, 3);
            (cands, attempts, fetched)
        }
    })
    .await;

    match searched {
        Ok((_cands, attempts, Ok((payload, code)))) => respond_ok(
            payload,
            code,
            track,
            started.elapsed().as_millis() as u64,
            attempts,
        ),
        Ok((_cands, attempts, Err((failure, mut inner)))) => {
            let mut all = attempts;
            all.append(&mut inner);
            respond_err(failure, 0, track, started.elapsed().as_millis() as u64, all)
        }
        Err(e) => respond_err(
            LyricsFailure::new(
                LyricsErrorKind::NetworkError,
                "lyrics",
                "歌词任务异常退出",
                format!("spawn_blocking join 失败：{e}"),
            ),
            0,
            track,
            started.elapsed().as_millis() as u64,
            Vec::new(),
        ),
    }
}

/// 选词窗口用：只搜索、打分、补预览，不自动选。
#[tauri::command]
pub async fn search_lyrics_candidates(
    title: String,
    artist: String,
    album: Option<String>,
    duration: Option<f64>,
) -> Result<Vec<Candidate>, String> {
    if title.trim().is_empty() {
        return Err("歌曲名为空".into());
    }
    let q = LyricsQuery {
        title,
        artist,
        album,
        duration,
    };
    tauri::async_runtime::spawn_blocking(move || {
        let (mut cands, _attempts) = collect_candidates(&q);
        enrich_previews(&mut cands, 5);
        cands
    })
    .await
    .map_err(|e| format!("搜索任务失败：{e}"))
}

/// 选词窗口点选后立刻拉这首的歌词
#[tauri::command]
pub async fn fetch_lyrics_for_candidate(cand: Candidate) -> LyricsResponse {
    let started = std::time::Instant::now();
    let track = format!("{} - {}", cand.title, cand.artist);
    let result = tauri::async_runtime::spawn_blocking(move || {
        match provider_by_name(&cand.source) {
            Some(p) => p.fetch(&cand),
            None => Err((
                LyricsFailure::new(LyricsErrorKind::AllFailed, "candidate", "该歌词源不可用", cand.source.clone()),
                0,
            )),
        }
    })
    .await;

    match result {
        Ok(Ok((payload, code))) => respond_ok(payload, code, track, started.elapsed().as_millis() as u64, Vec::new()),
        Ok(Err((f, code))) => respond_err(f, code, track, started.elapsed().as_millis() as u64, Vec::new()),
        Err(e) => respond_err(
            LyricsFailure::new(LyricsErrorKind::NetworkError, "lyrics", "歌词任务异常退出", format!("{e}")),
            0,
            track,
            started.elapsed().as_millis() as u64,
            Vec::new(),
        ),
    }
}

#[tauri::command]
pub fn get_lyric_binding(app: tauri::AppHandle, title: String, artist: String) -> Option<bindings::Binding> {
    bindings::get(&app, &matcher::track_key(&title, &artist))
}

#[tauri::command]
pub fn set_lyric_binding(
    app: tauri::AppHandle,
    title: String,
    artist: String,
    candidate: Candidate,
    offset_ms: i32,
) -> Result<(), String> {
    let key = matcher::track_key(&title, &artist);
    bindings::set(
        &app,
        &key,
        bindings::Binding {
            source: candidate.source,
            source_id: candidate.source_id,
            title: candidate.title,
            artist: candidate.artist,
            duration: candidate.duration,
            offset_ms,
            saved_at_ms: now_ms(),
        },
    )
}

#[tauri::command]
pub fn clear_lyric_binding(app: tauri::AppHandle, title: String, artist: String) -> Result<(), String> {
    bindings::remove(&app, &matcher::track_key(&title, &artist))
}

#[tauri::command]
pub fn list_lyric_bindings(app: tauri::AppHandle) -> Vec<(String, bindings::Binding)> {
    bindings::all(&app)
}

/// 诊断面板读：最近一次歌词请求的状态
#[tauri::command]
pub fn lyrics_status() -> Option<LyricsStatusEntry> {
    last_status()
}
