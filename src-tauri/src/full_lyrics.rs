//! 完整歌词窗口。
//!
//! 和岛体的关系：岛体是"只看当前一句"的常驻条，完整歌词是
//! "想找某一句 / 想跳到某个位置"时才打开的重窗口。
//!
//! 窗口属性刻意和岛体相反：
//!   - 有装饰（用户能拉大拉小、能关）
//!   - 不置顶（打开它通常是临时用一下，不想挡住别的东西）
//!   - 不透明（大面积文字需要清晰底，透明窗口在这个尺寸上看不清）

use tauri::{AppHandle, Manager, Runtime, WebviewUrl, WebviewWindowBuilder};

pub const LABEL: &str = "full-lyrics";

/// 打开（或聚焦）完整歌词窗口。
pub fn open<R: Runtime>(app: &AppHandle<R>) {
    if let Some(win) = app.get_webview_window(LABEL) {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        return;
    }

    let built = WebviewWindowBuilder::new(
        app,
        LABEL,
        WebviewUrl::App("index.html?window=full-lyrics".into()),
    )
    .title("Lyric Island · 完整歌词")
    .inner_size(520.0, 720.0)
    .min_inner_size(380.0, 420.0)
    .resizable(true)
    .decorations(true)
    .transparent(false)
    .always_on_top(false)
    .center()
    .devtools(true)
    .build();

    match built {
        Ok(win) => {
            let _ = win.set_focus();
        }
        Err(e) => eprintln!("[full-lyrics] 打开窗口失败：{e}"),
    }
}

#[tauri::command]
pub async fn open_full_lyrics(app: AppHandle) {
    open(&app);
}
