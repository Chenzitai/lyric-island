//! 网易云音乐歌词源。
//!
//! 走的是网页端公开接口（`music.163.com/api/...`），不是私有云 API：
//!
//! - 搜索：`/api/search/get/web`，返回 JSON，`result.songs[]` 里带 id / 歌名 /
//!   歌手数组 / 专辑 / 时长（毫秒）。
//! - 歌词：`/api/song/lyric?id=<id>&lv=-1&tv=-1&rv=-1&kv=-1`，
//!   一次能拿到四份：
//!     `lrc.lyric`     原文（带 `[mm:ss.xx]` 时间轴）
//!     `tlyric.lyric`  翻译
//!     `romalrc.lyric` 音译（罗马音 / 拼音，不是每首都有）
//!     `yrc.lyric`     逐字（`[mm:ss.mmm]字(时长,0)字(时长,0)`）
//!
//! 三个注意点：
//!
//! 1. 必须带 `Referer: https://music.163.com/`，否则经常返回空结果。
//! 2. `duration` 单位是**毫秒**，换成分要除 1000。
//! 3. 有的歌只有 `yrc` 没有 `lrc`（新歌逐字化之后），所以要两种都认。
//!    `yrc` 本身也是"带时间戳的原文"，只是时间戳是行首 + 逐字时长。

use std::time::Duration;

use serde::Deserialize;

use super::matcher::{normalize_title, Candidate};
use super::{LyricsErrorKind, LyricsFailure, LyricsPayload, LyricsProvider, LyricsQuery};

const NAME: &str = "netease";
const SEARCH_URL: &str = "https://music.163.com/api/search/get/web";
const LYRIC_URL: &str = "https://music.163.com/api/song/lyric";

pub struct NeteaseProvider {
    http: reqwest::blocking::Client,
}

impl NeteaseProvider {
    pub fn new() -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://music.163.com/"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
            ),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json, text/plain, */*"),
        );
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(6))
            .default_headers(headers)
            .build()
            .map_err(|e| format!("创建网易云 HTTP 客户端失败：{e}"))?;
        Ok(Self { http })
    }

    fn net_err(&self, e: reqwest::Error, step: &str) -> LyricsFailure {
        let msg = if e.is_timeout() {
            "网易云请求超时"
        } else if e.is_connect() {
            "连不上网易云"
        } else {
            "网易云网络请求失败"
        };
        LyricsFailure::new(LyricsErrorKind::NetworkError, NAME, msg, format!("{step}: {e}"))
    }
}

// ---------------- 搜索 ----------------

#[derive(Deserialize, Debug)]
struct SearchResp {
    #[serde(default)]
    result: Option<SearchResult>,
    #[serde(default)]
    code: i64,
}

#[derive(Deserialize, Debug)]
struct SearchResult {
    #[serde(default)]
    songs: Vec<Song>,
}

#[derive(Deserialize, Debug)]
struct Song {
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<Artist>,
    #[serde(default)]
    album: Album,
    #[serde(default)]
    duration: i64,
}

#[derive(Deserialize, Debug)]
struct Artist {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Debug, Default)]
struct Album {
    #[serde(default)]
    name: String,
}

// ---------------- 歌词 ----------------

#[derive(Deserialize, Debug)]
struct LyricResp {
    #[serde(default)]
    lrc: Option<LyricField>,
    #[serde(default)]
    tlyric: Option<LyricField>,
    #[serde(default)]
    romalrc: Option<LyricField>,
    #[serde(default)]
    yrc: Option<LyricField>,
}

#[derive(Deserialize, Debug, Default)]
struct LyricField {
    #[serde(default)]
    lyric: String,
}

impl LyricsProvider for NeteaseProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    fn priority(&self) -> i32 {
        30
    }

    fn search(&self, q: &LyricsQuery) -> Result<Vec<Candidate>, LyricsFailure> {
        let keyword = if q.artist.trim().is_empty() {
            q.title.clone()
        } else {
            format!("{} {}", q.title, q.artist)
        };

        let resp = self
            .http
            .get(SEARCH_URL)
            .query(&[
                ("s", keyword.as_str()),
                ("type", "1"),
                ("offset", "0"),
                ("limit", "15"),
                ("total", "true"),
                ("csrf_token", ""),
            ])
            .send()
            .map_err(|e| self.net_err(e, "/search"))?;

        let code = resp.status().as_u16();
        if code == 429 {
            return Err(LyricsFailure::new(
                LyricsErrorKind::RateLimited,
                NAME,
                "网易云限流了，稍后再试",
                "/search: HTTP 429",
            ));
        }
        if !resp.status().is_success() {
            return Err(LyricsFailure::new(
                LyricsErrorKind::NetworkError,
                NAME,
                "网易云搜索返回异常",
                format!("/search: HTTP {code}"),
            ));
        }

        let body = resp.json::<SearchResp>().map_err(|e| {
            LyricsFailure::new(
                LyricsErrorKind::ParseError,
                NAME,
                "网易云返回的内容不是预期格式",
                format!("/search 反序列化失败：{e}"),
            )
        })?;

        if body.code != 200 {
            return Err(LyricsFailure::new(
                LyricsErrorKind::NetworkError,
                NAME,
                "网易云搜索被拒绝",
                format!("/search: code={}", body.code),
            ));
        }

        let songs = body.result.map(|r| r.songs).unwrap_or_default();
        let want = normalize_title(&q.title);

        Ok(songs
            .into_iter()
            .map(|s| {
                let artist = s
                    .artists
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ");
                // 搜索阶段拿不到"有没有翻译/逐字"，但**能给 has_synced**：
                // 网易云只要拿到 song id 就几乎一定能取到 LRC，这是源的能力属性。
                //
                // 这个标记会直接进打分（有 synced +15）。设成 false 的话，
                // 网易云永远比 LRCLIB 低 15 分 —— 实测就变成"明明有带翻译的
                // 原词，却选了 LRCLIB 那份没翻译的"。宁可相信能力，不能漏掉翻译。
                Candidate {
                    source: NAME.into(),
                    source_id: s.id.to_string(),
                    title: s.name,
                    artist,
                    album: s.album.name,
                    duration: s.duration as f64 / 1000.0,
                    has_synced: true,
                    has_word_sync: false,
                    has_translation: false,
                    preview: String::new(),
                    score: 0,
                }
            })
            // 搜索接口经常把翻唱/伴奏排在前面，标题完全不像的直接踢掉
            .filter(|c| {
                let t = normalize_title(&c.title);
                t.contains(&want) || want.contains(&t)
            })
            .take(10)
            .collect())
    }

    fn fetch(&self, cand: &Candidate) -> Result<(LyricsPayload, u16), (LyricsFailure, u16)> {
        let resp = self
            .http
            .get(LYRIC_URL)
            .query(&[
                ("id", cand.source_id.as_str()),
                ("lv", "-1"),
                ("tv", "-1"),
                ("rv", "-1"),
                ("kv", "-1"),
            ])
            .send()
            .map_err(|e| (self.net_err(e, "/lyric"), 0))?;

        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err((
                LyricsFailure::new(
                    LyricsErrorKind::NetworkError,
                    NAME,
                    "网易云歌词接口返回异常",
                    format!("/lyric: HTTP {code}"),
                ),
                code,
            ));
        }

        let body = resp.json::<LyricResp>().map_err(|e| {
            (
                LyricsFailure::new(
                    LyricsErrorKind::ParseError,
                    NAME,
                    "网易云歌词不是预期格式",
                    format!("/lyric 反序列化失败：{e}"),
                ),
                code,
            )
        })?;

        let lrc = body.lrc.map(|l| l.lyric).unwrap_or_default();
        let yrc = body.yrc.map(|l| l.lyric).unwrap_or_default();
        let translated = body.tlyric.map(|l| l.lyric).unwrap_or_default();
        let romanized = body.romalrc.map(|l| l.lyric).unwrap_or_default();

        // 有 lrc 用 lrc，没有就退到 yrc（yrc 行首同样带 `[mm:ss.mmm]`）
        let synced = if lrc.trim().is_empty() { yrc.clone() } else { lrc.clone() };

        if synced.trim().is_empty() && translated.trim().is_empty() {
            return Err((
                LyricsFailure::new(
                    LyricsErrorKind::NotFound,
                    NAME,
                    "网易云有这首歌但没有歌词",
                    format!("/lyric: id={} 返回空", cand.source_id),
                ),
                code,
            ));
        }

        let line_count = synced
            .lines()
            .filter(|l| l.trim_start().starts_with('['))
            .count();

        // 有 yrc 就说明这首歌有逐字数据；lrc 为空时上面已经退回到 yrc
        let has_word_sync = !yrc.trim().is_empty();

        Ok((
            LyricsPayload {
                synced,
                plain: String::new(),
                translated,
                romanized,
                word_synced: yrc,
                instrumental: false,
                word_sync: has_word_sync,
                source: format!("netease:{}", cand.source_id),
                matched_title: cand.title.clone(),
                matched_artist: cand.artist.clone(),
                duration: cand.duration,
                line_count,
                from_binding: false,
            },
            code,
        ))
    }
}
