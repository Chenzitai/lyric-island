// Windows 下 release 不带控制台窗口，入口就一行
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    lyric_island_lib::run()
}
