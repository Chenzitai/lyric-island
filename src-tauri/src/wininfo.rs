//! Win32 窗口枚举 —— 给"窗口标题回退通道"用。
//!
//! 为什么需要这个：酷狗音乐虽然接了 SMTC，但部分版本/配置下只给歌名不给进度，
//! 甚至完全不上报会话。Lyricify、Lyricod 这类工具普遍采用
//! **SMTC 优先 + 窗口标题兜底** 的双引擎，这里实现后者。
//!
//! 只枚举**可见且有标题**的顶层窗口，然后带上进程名和窗口类名 ——
//! 进程名用来确认"这真的是酷狗"，类名在进程名拿不到时作为第二判据。

use serde::Serialize;
use windows::core::{BOOL, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, MAX_PATH};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible,
};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub title: String,
    /// 小写进程名，例如 "kugou.exe"；拿不到时为空
    pub process: String,
    pub class: String,
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
    if IsWindowVisible(hwnd).as_bool() && GetWindowTextLengthW(hwnd) > 0 {
        let list = &mut *(lparam.0 as *mut Vec<HWND>);
        list.push(hwnd);
    }
    BOOL(1)
}

fn window_text(hwnd: HWND) -> String {
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; (len + 1) as usize];
    let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

fn process_name(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    unsafe {
        let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
            Ok(h) => h,
            Err(_) => return String::new(),
        };

        let mut buf = [0u16; MAX_PATH as usize];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
        .is_ok();
        let _ = CloseHandle(handle);

        if !ok || size == 0 {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..size as usize]);
        // 只留文件名，"C:\Program Files\KuGou\KuGou.exe" -> "kugou.exe"
        path.rsplit(|c| c == '\\' || c == '/')
            .next()
            .unwrap_or("")
            .to_lowercase()
    }
}

/// 枚举所有可见顶层窗口。失败返回空表，不 panic —— 这是辅助通道，挂了也不该拖垮主流程。
pub fn list_visible_windows() -> Vec<WindowInfo> {
    let mut handles: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut handles as *mut _ as isize));
    }

    handles
        .into_iter()
        .map(|hwnd| {
            let mut pid = 0u32;
            unsafe {
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
            }
            WindowInfo {
                title: window_text(hwnd),
                process: process_name(pid),
                class: class_name(hwnd),
            }
        })
        .filter(|w| !w.title.is_empty())
        .collect()
}
