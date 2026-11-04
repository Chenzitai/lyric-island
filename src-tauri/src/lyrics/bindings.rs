//! 用户手动选词的绑定记忆。
//!
//! 存在独立的 `lyric-bindings.json`，**不放进 settings.json**：
//! 绑定会随时间累积到几十上百条，混进设置文件会让每次改设置都重写一大坨数据。
//!
//! key 用 `matcher::track_key(title, artist)` = 归一化标题 + 主艺术家。
//! 故意不含时长 —— 同一首歌不同音源时长差几秒很常见，带时长会让绑定失效。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Runtime};
use tauri_plugin_store::StoreExt;

const STORE_FILE: &str = "lyric-bindings.json";
const STORE_KEY: &str = "bindings";

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Binding {
    pub source: String,
    pub source_id: String,
    pub title: String,
    pub artist: String,
    pub duration: f64,
    /// 这首歌专用的歌词偏移（ms），叠加在全局偏移之上
    pub offset_ms: i32,
    pub saved_at_ms: f64,
}

fn load_all<R: Runtime>(app: &AppHandle<R>) -> serde_json::Map<String, serde_json::Value> {
    (|| -> Option<serde_json::Map<String, serde_json::Value>> {
        let store = app.store(STORE_FILE).ok()?;
        let value = store.get(STORE_KEY)?;
        value.as_object().cloned()
    })()
    .unwrap_or_default()
}

fn save_all<R: Runtime>(app: &AppHandle<R>, map: &serde_json::Map<String, serde_json::Value>) -> Result<(), String> {
    let store = app.store(STORE_FILE).map_err(|e| format!("打开绑定存储失败：{e}"))?;
    store.set(STORE_KEY, serde_json::Value::Object(map.clone()));
    store.save().map_err(|e| format!("保存绑定失败：{e}"))
}

pub fn get<R: Runtime>(app: &AppHandle<R>, track_key: &str) -> Option<Binding> {
    let map = load_all(app);
    let v = map.get(track_key)?;
    serde_json::from_value(v.clone()).ok()
}

pub fn set<R: Runtime>(app: &AppHandle<R>, track_key: &str, b: Binding) -> Result<(), String> {
    let mut map = load_all(app);
    let v = serde_json::to_value(&b).map_err(|e| format!("序列化绑定失败：{e}"))?;
    map.insert(track_key.to_string(), v);
    save_all(app, &map)
}

pub fn remove<R: Runtime>(app: &AppHandle<R>, track_key: &str) -> Result<(), String> {
    let mut map = load_all(app);
    if map.remove(track_key).is_none() {
        return Ok(());
    }
    save_all(app, &map)
}

pub fn all<R: Runtime>(app: &AppHandle<R>) -> Vec<(String, Binding)> {
    let map = load_all(app);
    map.into_iter()
        .filter_map(|(k, v)| serde_json::from_value::<Binding>(v).ok().map(|b| (k, b)))
        .collect()
}

/// 清理掉太老的绑定（默认 180 天），避免文件无限增长。
#[allow(dead_code)]
pub fn prune<R: Runtime>(app: &AppHandle<R>, older_than: Duration) {
    let map = load_all(app);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0);
    let limit = older_than.as_millis() as f64;

    let mut kept = serde_json::Map::new();
    for (k, v) in map {
        let saved = v
            .get("savedAtMs")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        if now - saved < limit {
            kept.insert(k, v);
        }
    }
    let _ = save_all(app, &kept);
}
