//! 曲目匹配：标题归一化、艺术家拆分、时长校验、打分。
//!
//! 这一层不碰网络，纯字符串和时间比较，方便单独验证。
//!
//! 设计取舍：
//!
//! 1. **归一化只用于比较，不改显示**。`matched_title` 仍然是源返回的原始标题，
//!    归一化后的串只在内部用，免得用户看到「歌名被吃掉括号」的怪异现象。
//!
//! 2. **括号内容整体丢弃**。`(Live)` / `（伴奏）` / `[Remastered]` / `【影视版】`
//!    这些对匹配全是噪声。极少数歌名本身带括号含义（如《(Nothing But) Flowers》），
//!    丢弃后会变得过短，这时靠"标题包含关系"和时长来兜。
//!
//! 3. **时长 > ±3 秒直接丢弃**是硬规则，宁可不显示也不显示错的。但只在
//!    双方都有可信时长（> 5 秒）时才生效，SMTC 拿不到时长时不能误杀。

use serde::{Deserialize, Serialize};

/// 一个候选结果。既用于内部打分，也直接序列化给"选择歌词"窗口。
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    /// 源标识：lrclib / netease / qq
    pub source: String,
    /// 源内 ID，回取歌词时用
    pub source_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    /// 秒。0 表示源没给
    pub duration: f64,
    pub has_synced: bool,
    pub has_word_sync: bool,
    pub has_translation: bool,
    /// 前两行歌词，给选择窗口预览用
    pub preview: String,
    /// 归一化 + 打分的中间态，最终分存在这里
    pub score: i32,
}

/// 全角转半角。日文/中文输入法常见的 `（）：` 会导致匹配失败。
fn to_halfwidth(c: char) -> char {
    match c {
        '\u{3000}' => ' ',
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        _ => c,
    }
}

fn halfwidth_str(s: &str) -> String {
    s.chars().map(to_halfwidth).collect()
}

/// 去掉括号里的内容，支持嵌套与中英文括号混用。
fn strip_brackets(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' | '<' | '「' | '『' | '【' => depth += 1,
            ')' | ']' | '}' | '>' | '」' | '』' | '】' => {
                if depth > 0 {
                    depth -= 1;
                }
            }
            _ => {
                if depth == 0 {
                    out.push(ch);
                }
            }
        }
    }
    out
}

/// 截断到第一个 ` - ` 之前。`Track - Remastered 2011` / `歌名 - 歌手` 都会命中。
fn cut_dash_suffix(s: &str) -> String {
    match s.find(" - ") {
        Some(i) if i > 0 => s[..i].to_string(),
        _ => s.to_string(),
    }
}

/// 截断到 feat. / ft. / with 之前。
fn cut_feat(s: &str) -> String {
    let needles = ["feat.", "feat ", "ft.", " ft ", " with ", "&"];
    let mut cut = s.len();
    for n in needles {
        if let Some(i) = s.find(n) {
            if i < cut {
                cut = i;
            }
        }
    }
    s[..cut].to_string()
}

/// 标题归一化：小写、去括号、去 feat、去 ` - ` 后缀、只留字母数字汉字。
pub fn normalize_title(raw: &str) -> String {
    let s = halfwidth_str(raw).to_lowercase();
    let s = strip_brackets(&s);
    let s = cut_feat(&s);
    let s = cut_dash_suffix(&s);
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .trim()
        .to_string()
}

/// 艺术家拆分：`A & B` / `A, B` / `A、B` / `A feat. B` → 多个归一化名字。
///
/// 保留第一个作为"主艺术家"（顺序通常是主次之分）。
pub fn split_artists(raw: &str) -> Vec<String> {
    let s = halfwidth_str(raw).to_lowercase();
    let s = strip_brackets(&s);

    let mut parts: Vec<String> = Vec::new();
    // feat. 之后单独算一个
    for seg in s.split("feat.") {
        for seg2 in seg.split(" ft. ") {
            for seg3 in seg2.split(['&', ',', '/', '、', ';', '|', '，'].as_ref()) {
                let cleaned: String = seg3.chars().filter(|c| c.is_alphanumeric()).collect();
                let cleaned = cleaned.trim().to_string();
                if !cleaned.is_empty() {
                    parts.push(cleaned);
                }
            }
        }
    }
    parts.dedup();
    parts
}

/// 时长是否可信（源和 SMTC 都给了才比较）。
pub fn duration_ok(candidate: f64, target: Option<f64>) -> bool {
    let Some(t) = target.filter(|d| *d > 5.0) else {
        return true; // SMTC 没给时长，不设限
    };
    if candidate <= 5.0 {
        return true; // 源没给时长，不设限
    }
    (candidate - t).abs() <= 3.0
}

/// 给一个候选打分。分数越高越优先。
///
/// 权重设计的出发点：**标题 > 艺术家 > 时长**。标题错了整首歌就错了，
/// 艺术家错了可能只是版本差异，时长差个几百毫秒很正常。
pub fn score_candidate(cand: &Candidate, q_title: &str, q_artists: &[String], q_duration: Option<f64>) -> i32 {
    let mut score = 0i32;

    let cand_title = normalize_title(&cand.title);
    let want_title = normalize_title(q_title);

    if !want_title.is_empty() && !cand_title.is_empty() {
        if cand_title == want_title {
            score += 50;
        } else if cand_title.contains(&want_title) || want_title.contains(&cand_title) {
            // 包含关系给一半分，避免"歌名 A"命中"歌名 A 之 B 完整版"
            score += 25;
        } else {
            // 标题完全不像，直接判死
            return -1000;
        }
    }

    let cand_artists = split_artists(&cand.artist);
    if !q_artists.is_empty() && !cand_artists.is_empty() {
        let exact = q_artists.iter().any(|a| cand_artists.iter().any(|b| a == b));
        if exact {
            score += 30;
        } else if q_artists
            .iter()
            .any(|a| cand_artists.iter().any(|b| b.contains(a) || a.contains(b)))
        {
            score += 15;
        } else {
            // 艺术家不匹配是强负信号，但不判死 —— 有些源把 artist 写成专辑名
            score -= 20;
        }
    }

    if let Some(target) = q_duration.filter(|d| *d > 5.0) {
        if cand.duration > 5.0 {
            let diff = (cand.duration - target).abs();
            if diff <= 1.0 {
                score += 20;
            } else if diff <= 2.0 {
                score += 12;
            } else if diff <= 3.0 {
                score += 6;
            }
        }
    }

    if cand.has_synced {
        score += 15;
    }
    if cand.has_word_sync {
        score += 5;
    }
    if cand.has_translation {
        score += 5;
    }

    score
}

/// 绑定记忆用的 key：标题归一化 + 主艺术家。
///
/// **不含时长** —— 同一首歌不同音源时长差几秒很常见，带上时长会导致绑定失效。
pub fn track_key(title: &str, artist: &str) -> String {
    let t = normalize_title(title);
    let a = split_artists(artist).into_iter().next().unwrap_or_default();
    if a.is_empty() {
        t
    } else {
        format!("{t}|{a}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_strips_noise() {
        assert_eq!(normalize_title("Into You (Remastered 2011)"), "intoyou");
        assert_eq!(normalize_title("晴天 - Live"), "晴天");
        assert_eq!(normalize_title("Song feat. Someone"), "song");
        assert_eq!(normalize_title("【中文字幕】歌名"), "歌名");
        assert_eq!(normalize_title("Ａ Ｂ　Ｃ"), "abc");
    }

    #[test]
    fn artists_split() {
        assert_eq!(split_artists("Ariana Grande"), vec!["arianagrande"]);
        assert_eq!(split_artists("A & B"), vec!["a", "b"]);
        assert_eq!(split_artists("周杰伦、方文山"), vec!["周杰伦", "方文山"]);
        assert_eq!(split_artists("A feat. B"), vec!["a", "b"]);
    }

    #[test]
    fn duration_gate() {
        assert!(duration_ok(200.0, Some(202.0)));
        assert!(!duration_ok(200.0, Some(210.0)));
        assert!(duration_ok(200.0, None));
        assert!(duration_ok(0.0, Some(210.0)));
    }
}
