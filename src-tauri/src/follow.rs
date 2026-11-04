//! 桌面歌词的「跟随播放」自动收放。
//!
//! 判定放在 Rust 侧而不是前端，原因有三：
//!   1. 前端窗口可能根本没打开（桌面歌词默认隐藏），没窗口就没监听者；
//!   2. 用户关掉岛体后，前端也不再跑 rAF，时钟停了就没法判"暂停多久了"；
//!   3. SMTC 的权威状态本来就在这个 watcher 线程里，就近判断最省事。
//!
//! 规则（每 500ms 走一帧）：
//!   - 检测到"开始播放"的**上升沿**（上一帧没在播，这一帧在播）→ 显示
//!   - 非播放态（暂停 / 停止 / 会话消失）持续 3 秒 → 隐藏
//!   - 关掉开关 → 如果当前这份是"自动打出来的"，收回
//!
//! 为什么必须用上升沿而不是"playing 就一直显示"：
//! 用户手动把桌面歌词关掉时，如果只看 playing 状态，下一帧就会给他弹回来 ——
//! 手动关闭等于无效。用上升沿的话，手动关掉后本曲内不会再弹，直到下次暂停恢复。
//!
//! 关键约束：**自动收放永远不改 `desktop_lyrics_visible`**。
//! 那个字段代表用户的持久化意图，被自动行为改写的话，下次启动就会出现
//! "我明明手动关掉了，它又自己冒出来"这种鬼故事。

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Runtime};

/// 暂停多久之后才收起。给用户留出"暂停一下看看歌词"的余地，
/// 也避免播放器抖动（缓冲、换曲间奏）造成反复收放。
const IDLE_HIDE_AFTER_MS: u64 = 3000;

#[derive(Default)]
struct State {
    /// 当前这次显示是不是自动打出来的
    auto_shown: bool,
    /// 上一帧是否处于"可跟随的播放中"（用来判断上升沿）
    was_playing: bool,
    /// 从什么时候开始处于非播放态（用来算 3 秒）
    idle_since: Option<Instant>,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

enum Action {
    Show,
    Hide,
    None,
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut guard = STATE.lock().ok()?;
    let st = guard.get_or_insert_with(State::default);
    Some(f(st))
}

/// 喂一帧播放状态，决定要不要收放桌面歌词。
///
/// 由 SMTC watcher 每帧调用；外部不需要关心返回值。
pub fn tick<R: Runtime>(app: &AppHandle<R>, np: &crate::smtc::NowPlaying) {
    let cfg = crate::settings::get();

    // 持锁期间只做状态机计算，动作留到锁外执行 ——
    // auto_show / auto_hide 会碰窗口 API，可能反向往回要设置，别再套一层锁。
    let action = match with_state(|st| {
        // ---- 开关被关掉：把自动显示的收回去，用户手动开的保留 ----
        if !cfg.desktop_lyrics_follow_playback {
            st.idle_since = None;
            st.was_playing = false;
            if st.auto_shown {
                st.auto_shown = false;
                return Action::Hide;
            }
            return Action::None;
        }

        let is_video = np.playback_type.eq_ignore_ascii_case("video");
        // 标题为空说明这一帧"读到了会话但没拿到元信息"，
        // 这时候弹一个空白桌面歌词没意义。
        let playing = np.active && np.playing && !is_video && !np.title.trim().is_empty();

        let rising = playing && !st.was_playing;
        st.was_playing = playing;

        if playing {
            st.idle_since = None;
            // 已经在显示就别重复 show —— 每帧 show 一次会让窗口闪烁
            if rising && !st.auto_shown {
                st.auto_shown = true;
                return Action::Show;
            }
            return Action::None;
        }

        let since = *st.idle_since.get_or_insert_with(Instant::now);
        if st.auto_shown && since.elapsed() >= Duration::from_millis(IDLE_HIDE_AFTER_MS) {
            st.auto_shown = false;
            st.idle_since = None;
            return Action::Hide;
        }
        Action::None
    }) {
        Some(a) => a,
        None => return,
    };

    match action {
        Action::Show => crate::desktop_lyrics::auto_show(app),
        Action::Hide => crate::desktop_lyrics::auto_hide(app),
        Action::None => {}
    }
}

/// 用户手动在托盘里切换了「桌面歌词」。
///
/// 手动关掉时必须清掉 `auto_shown`，否则状态机会以为"还显示着"，
/// 下次该弹的时候反而不弹了。
pub fn note_manual(visible: bool) {
    with_state(|st| {
        if !visible {
            st.auto_shown = false;
        }
    });
}
