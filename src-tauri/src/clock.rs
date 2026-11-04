//! 会话级时钟：Rust 这边是进度的唯一权威。
//!
//! 前端不再自己"从 0 开始数"，而是向这里要进度。这解决了三件事：
//!
//! 1. **刷新后从头滚** —— 前端重建时 `now_playing` 会带上实时外推过的位置，
//!    而不是最后一次推送时那个已经过期的快照。
//! 2. **无时间轴会话（酷狗）** —— 播放器报的 Position 恒为 0，这里靠
//!    "锚点 + 经过时间"外推，暂停时把外推值钉死。
//! 3. **主动检测 Seek** —— 播放器不提供拖动事件，那就自己比对：
//!    实际位置与"按上次锚点推算的位置"差得太多，就是用户拖了进度条。
//!
//! 阈值取 -800ms / +1500ms 而不是严格相等：SMTC 报的位置本身有抖动，
//! 网络/调度也会让采样间隔不稳定，卡太死会天天误判成拖动。

use std::sync::Mutex;
use std::time::Instant;

use serde::Serialize;

use crate::smtc::NowPlaying;

/// 给前端的进度快照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub position_ms: f64,
    pub duration_ms: f64,
    pub playing: bool,
    pub has_timeline: bool,
    pub app_id: String,
    pub title: String,
    pub artist: String,
    /// 累计检测到的 seek 次数，前端可以据此判断"这不是普通心跳"
    pub seek_count: u64,
}

impl Progress {
    fn empty() -> Self {
        Self {
            position_ms: 0.0,
            duration_ms: 0.0,
            playing: false,
            has_timeline: false,
            app_id: String::new(),
            title: String::new(),
            artist: String::new(),
            seek_count: 0,
        }
    }
}

struct Inner {
    app_id: String,
    title: String,
    artist: String,
    /// 锚点：最后一次可信的进度
    anchor_ms: f64,
    /// 锚点时刻
    anchor_at: Instant,
    playing: bool,
    has_timeline: bool,
    duration_ms: f64,
    seek_count: u64,
}

static INNER: Mutex<Option<Inner>> = Mutex::new(None);

/// 位置倒退超过这个量 → 认为用户往回拖了
const SEEK_BACK_MS: f64 = 800.0;
/// 位置比预测超前超过这个量 → 认为用户往后拖了
const SEEK_FWD_MS: f64 = 1500.0;

fn extrapolate(g: &Inner) -> f64 {
    if !g.playing {
        return g.anchor_ms;
    }
    let elapsed = g.anchor_at.elapsed().as_millis() as f64;
    let mut p = g.anchor_ms + elapsed;
    // 别越过结尾：有些播放器在曲末会一直报同一个位置，外推会冲出去
    if g.duration_ms > 1000.0 && p > g.duration_ms {
        p = g.duration_ms;
    }
    p
}

/// watcher 每帧（500ms）喂一次。
/// 返回 `Some(位置)` 表示这一帧检测到用户拖了进度条。
pub fn tick(np: &NowPlaying) -> Option<f64> {
    let now = Instant::now();
    let mut guard = match INNER.lock() {
        Ok(g) => g,
        Err(_) => return None,
    };

    // 没有会话：清空，下次从新会话的第一帧重新锚定
    if !np.active {
        *guard = None;
        return None;
    }

    let same_track = guard
        .as_ref()
        .map(|g| g.title == np.title && g.artist == np.artist && g.app_id == np.app_id)
        .unwrap_or(false);

    if !same_track {
        *guard = Some(Inner {
            app_id: np.app_id.clone(),
            title: np.title.clone(),
            artist: np.artist.clone(),
            anchor_ms: np.position_ms.max(0.0),
            anchor_at: now,
            playing: np.playing,
            has_timeline: np.has_timeline,
            duration_ms: np.duration_ms,
            seek_count: 0,
        });
        return None;
    }

    let g = guard.as_mut().unwrap();
    let predicted = extrapolate(g);
    let was_playing = g.playing;

    g.has_timeline = np.has_timeline;
    g.duration_ms = np.duration_ms;

    // 暂停：把外推值钉死，否则 elapsed 会一直涨、歌词一直往前跑
    if was_playing && !np.playing {
        g.anchor_ms = predicted;
        g.anchor_at = now;
        g.playing = false;
        return None;
    }

    // 从暂停恢复：有真值就用真值，没有就接着上次算出来的位置往下走
    if !was_playing && np.playing {
        g.anchor_ms = if np.has_timeline && np.position_ms > 0.0 {
            np.position_ms
        } else {
            predicted
        };
        g.anchor_at = now;
        g.playing = true;
        return None;
    }

    // 无时间轴：播放器报的 0 是假的，只能靠自己累加
    if !np.has_timeline {
        g.anchor_ms = predicted;
        g.anchor_at = now;
        g.playing = np.playing;
        return None;
    }

    let diff = np.position_ms - predicted;
    let is_seek = diff < -SEEK_BACK_MS || diff > SEEK_FWD_MS;

    g.anchor_ms = np.position_ms.max(0.0);
    g.anchor_at = now;
    g.playing = np.playing;

    if is_seek {
        g.seek_count += 1;
        eprintln!(
            "[clock] 检测到 seek：预测 {:.0}ms → 实际 {:.0}ms（差 {:+.0}ms）",
            predicted, np.position_ms, diff
        );
        return Some(np.position_ms.max(0.0));
    }

    None
}

/// 当前进度（已按播放状态外推）。前端刷新时调它，不要从 0 开始。
pub fn progress() -> Progress {
    let guard = match INNER.lock() {
        Ok(g) => g,
        Err(_) => return Progress::empty(),
    };
    match guard.as_ref() {
        Some(g) => Progress {
            position_ms: extrapolate(g),
            duration_ms: g.duration_ms,
            playing: g.playing,
            has_timeline: g.has_timeline,
            app_id: g.app_id.clone(),
            title: g.title.clone(),
            artist: g.artist.clone(),
            seek_count: g.seek_count,
        },
        None => Progress::empty(),
    }
}

/// 用户主动 seek 之后调用：把锚点直接挪到目标位置。
///
/// 不这么做的话，下一帧 `tick()` 会拿播放器报来的新位置和自己预测的旧位置比，
/// 又把这次跳转判成一次"检测到的 seek"，重复 emit 一遍事件。
pub fn force_resync(pos_ms: f64) {
    if let Ok(mut g) = INNER.lock() {
        if let Some(inner) = g.as_mut() {
            inner.anchor_ms = pos_ms.max(0.0);
            inner.anchor_at = std::time::Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn np(title: &str, pos: f64, dur: f64, playing: bool, has_tl: bool) -> NowPlaying {
        NowPlaying {
            active: true,
            app_id: "test.app".into(),
            title: title.into(),
            artist: "tester".into(),
            playing,
            position_ms: pos,
            duration_ms: dur,
            has_timeline: has_tl,
            ..Default::default()
        }
    }

    fn sleep_ms(ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    /// 这三个用例共用同一个全局 static，必须串行跑：
    /// `cargo test clock -- --test-threads=1`
    #[test]
    fn detects_seek_both_directions() {
        // 第一帧：建锚点
        let _ = tick(&np("seek", 10_000.0, 240_000.0, true, true));

        // 正常推进 600ms，位置对得上，不该判 seek
        sleep_ms(600);
        assert!(
            tick(&np("seek", 10_600.0, 240_000.0, true, true)).is_none(),
            "正常推进不该被判成 seek"
        );

        // 往回拖到 40 秒：实际远小于预测值
        let back = tick(&np("seek", 40_000.0, 240_000.0, true, true));
        assert_eq!(back, Some(40_000.0), "往回拖必须判 seek");

        // 往前拖到 100 秒：实际远大于预测值
        let fwd = tick(&np("seek", 100_000.0, 240_000.0, true, true));
        assert_eq!(fwd, Some(100_000.0), "往前拖必须判 seek");
    }

    #[test]
    fn extrapolates_when_no_timeline() {
        // 酷狗这类：播放器报的位置恒为 0，只能靠外推
        let _ = tick(&np("notl", 0.0, 0.0, true, false));
        sleep_ms(700);
        let _ = tick(&np("notl", 0.0, 0.0, true, false));

        let p = progress().position_ms;
        assert!(
            (500.0..1300.0).contains(&p),
            "无时间轴应外推到 700ms 附近，实际 {p}"
        );
    }

    #[test]
    fn pause_freezes_progress() {
        let _ = tick(&np("pause", 5000.0, 0.0, true, false));
        sleep_ms(400);
        let _ = tick(&np("pause", 5000.0, 0.0, false, false));

        let p1 = progress().position_ms;
        sleep_ms(500);
        let p2 = progress().position_ms;
        assert!(
            (p2 - p1).abs() < 60.0,
            "暂停后进度不该继续涨：{p1} → {p2}"
        );
    }
}
