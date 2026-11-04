//! QQ 音乐歌词源。
//!
//! 接口都是网页端在用的公开接口：
//!
//! - 搜索：`https://c.y.qq.com/soso/fcgi-bin/client_search_cp`
//! - 歌词：`https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg`
//!
//! 两个必须注意的地方：
//!
//! 1. **必须带 `Referer: https://y.qq.com/`**，否则直接返回空。
//! 2. 歌词正文是 **HTML 实体编码**的（`&#58;` = `:`、`&#10;` = 换行），
//!    不解码的话时间戳会变成 `[00&#58;12.34]`，前端正则一行都匹配不上。
//!
//! 逐字（QRC）这次不接：QQ 的逐字走 `lyric_download.fcg?lrctype=4`，
//! 返回加密 XML，要单独逆向，优先级放到最后（用户在需求里也是这么排的）。

use std::time::Duration;

use serde::Deserialize;

use super::matcher::{normalize_title, Candidate};
use super::{LyricsErrorKind, LyricsFailure, LyricsPayload, LyricsProvider, LyricsQuery};

const NAME: &str = "qq";
const SEARCH_URL: &str = "https://c.y.qq.com/soso/fcgi-bin/client_search_cp";
const LYRIC_URL: &str = "https://c.y.qq.com/lyric/fcgi-bin/fcg_query_lyric_new.fcg";

pub struct QqProvider {
    http: reqwest::blocking::Client,
}

impl QqProvider {
    pub fn new() -> Result<Self, String> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static("https://y.qq.com/"),
        );
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/124.0 Safari/537.36",
            ),
        );
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(6))
            .default_headers(headers)
            .build()
            .map_err(|e| format!("创建 QQ 音乐 HTTP 客户端失败：{e}"))?;
        Ok(Self { http })
    }

    fn net_err(&self, e: reqwest::Error, step: &str) -> LyricsFailure {
        let msg = if e.is_timeout() {
            "QQ 音乐请求超时"
        } else if e.is_connect() {
            "连不上 QQ 音乐"
        } else {
            "QQ 音乐网络请求失败"
        };
        LyricsFailure::new(LyricsErrorKind::NetworkError, NAME, msg, format!("{step}: {e}"))
    }
}

#[derive(Deserialize, Debug)]
struct SearchResp {
    #[serde(default)]
    data: Option<SearchData>,
    #[serde(default)]
    code: i64,
}

#[derive(Deserialize, Debug)]
struct SearchData {
    #[serde(default)]
    song: Option<SongBlock>,
}

#[derive(Deserialize, Debug)]
struct SongBlock {
    #[serde(default)]
    list: Vec<Song>,
}

#[derive(Deserialize, Debug)]
struct Song {
    #[serde(default, rename = "songmid")]
    song_mid: String,
    #[serde(default, rename = "songname")]
    song_name: String,
    #[serde(default)]
    singer: Vec<Singer>,
    #[serde(default, rename = "albumname")]
    album_name: String,
    #[serde(default)]
    interval: i64,
}

#[derive(Deserialize, Debug)]
struct Singer {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Debug)]
struct LyricResp {
    #[serde(default)]
    lyric: String,
    #[serde(default)]
    trans: String,
    #[serde(default)]
    retcode: i64,
}

/// QQ 返回的歌词是 HTML 实体编码。不全量实现 HTML 规范，
/// 只处理歌词里真正会出现的这几类。
fn unescape_html(s: &str) -> String {
    s.replace("&#58;", ":")
        .replace("&#46;", ".")
        .replace("&#39;", "'")
        .replace("&#10;", "\n")
        .replace("&#13;", "")
        .replace("&apos;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

impl LyricsProvider for QqProvider {
    fn name(&self) -> &'static str {
        NAME
    }

    fn priority(&self) -> i32 {
        20
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
                ("p", "1"),
                ("n", "15"),
                ("w", keyword.as_str()),
                ("format", "json"),
                // 不要加 new_json=1：新格式改字段名为 mid / name / album 对象，
                // 老格式才是 songmid / songname / albumname（字符串），解析简单得多。
                ("cr", "1"),
                ("t", "0"),
                ("g_tk", "5381"),
            ])
            .send()
            .map_err(|e| self.net_err(e, "/search"))?;

        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err(LyricsFailure::new(
                LyricsErrorKind::NetworkError,
                NAME,
                "QQ 音乐搜索返回异常",
                format!("/search: HTTP {code}"),
            ));
        }

        let body = resp.json::<SearchResp>().map_err(|e| {
            LyricsFailure::new(
                LyricsErrorKind::ParseError,
                NAME,
                "QQ 音乐返回的内容不是预期格式",
                format!("/search 反序列化失败：{e}"),
            )
        })?;

        if body.code != 0 {
            return Err(LyricsFailure::new(
                LyricsErrorKind::NetworkError,
                NAME,
                "QQ 音乐搜索被拒绝",
                format!("/search: code={}", body.code),
            ));
        }

        let list = body
            .data
            .and_then(|d| d.song)
            .map(|s| s.list)
            .unwrap_or_default();
        let want = normalize_title(&q.title);

        Ok(list
            .into_iter()
            .filter(|s| !s.song_mid.is_empty())
            .map(|s| {
                let artist = s
                    .singer
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ");
                // 同 netease：有 songmid 就能取到 LRC，has_synced 是源的能力，
                // 不是搜索结果自带的属性。设 false 会让 QQ 在打分里白丢 15 分。
                Candidate {
                    source: NAME.into(),
                    source_id: s.song_mid,
                    title: s.song_name,
                    artist,
                    album: s.album_name,
                    duration: s.interval as f64,
                    has_synced: true,
                    has_word_sync: false,
                    has_translation: false,
                    preview: String::new(),
                    score: 0,
                }
            })
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
                ("songmid", cand.source_id.as_str()),
                ("format", "json"),
                ("nobase64", "1"),
                ("g_tk", "5381"),
            ])
            .send()
            .map_err(|e| (self.net_err(e, "/lyric"), 0))?;

        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            return Err((
                LyricsFailure::new(
                    LyricsErrorKind::NetworkError,
                    NAME,
                    "QQ 音乐歌词接口返回异常",
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
                    "QQ 音乐歌词不是预期格式",
                    format!("/lyric 反序列化失败：{e}"),
                ),
                code,
            )
        })?;

        if body.retcode != 0 {
            return Err((
                LyricsFailure::new(
                    LyricsErrorKind::NotFound,
                    NAME,
                    "QQ 音乐没有这首歌的歌词",
                    format!("/lyric: retcode={}", body.retcode),
                ),
                code,
            ));
        }

        let synced = unescape_html(&body.lyric);
        let translated = unescape_html(&body.trans);

        if synced.trim().is_empty() {
            return Err((
                LyricsFailure::new(
                    LyricsErrorKind::NotFound,
                    NAME,
                    "QQ 音乐没有这首歌的歌词",
                    format!("/lyric: songmid={} 返回空", cand.source_id),
                ),
                code,
            ));
        }

        let line_count = synced
            .lines()
            .filter(|l| l.trim_start().starts_with('['))
            .count();

        Ok((
            LyricsPayload {
                synced,
                plain: String::new(),
                translated,
                romanized: String::new(),
                word_synced: String::new(),
                instrumental: false,
                word_sync: false,
                source: format!("qq:{}", cand.source_id),
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
