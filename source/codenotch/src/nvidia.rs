//! NVIDIA NIM API usage adapter.
//! Endpoint: GET https://integrate.api.nvidia.com/v1/models
//! Header: Authorization: Bearer <token>

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://integrate.api.nvidia.com/v1/models";
const POLL_SECS: u64 = 300;

static REFRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn request_refresh() {
    REFRESH.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn store_path() -> PathBuf {
    crate::config::config_path().with_file_name("nvidia.json")
}

pub fn load_persisted() -> UsageSnapshot {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|t| serde_json::from_str::<UsageSnapshot>(&t).ok())
        .map(|mut s| {
            if !s.windows.is_empty() {
                s.status = "stale".into();
            }
            s
        })
        .unwrap_or_default()
}

fn persist(s: &UsageSnapshot) {
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(store_path(), t);
    }
}

pub fn find_api_key(cfg_key: &str) -> Option<String> {
    let trimmed = cfg_key.trim();
    if !trimmed.is_empty() {
        return Some(trimmed.to_string());
    }
    if let Ok(env_key) = std::env::var("NVIDIA_API_KEY").or_else(|_| std::env::var("NVAPI_KEY")) {
        let env_trimmed = env_key.trim();
        if !env_trimmed.is_empty() {
            return Some(env_trimmed.to_string());
        }
    }
    None
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ModelEntry {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResp {
    #[serde(default)]
    data: Vec<ModelEntry>,
}

pub fn parse_models_json(json: &str) -> Result<UsageSnapshot, String> {
    let resp: ModelsResp = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let count = resp.data.len();
    let window = LimitWindow {
        id: "nim".into(),
        label: format!("NVIDIA NIM ({count} models)"),
        used: 0.0,
        resets_at: None,
        count: None,
        derived: false,
    };
    Ok(UsageSnapshot {
        status: "ok".into(),
        windows: vec![window],
        fetched_at: now_ms(),
        note: format!("NVIDIA NIM · {count} models available"),
        backoff_until: 0,
    })
}

pub fn fetch_usage(key: &str) -> Result<UsageSnapshot, String> {
    let agent = ureq::builder()
        .timeout(Duration::from_secs(15))
        .build();

    let resp = agent
        .get(ENDPOINT)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Accept", "application/json")
        .call();

    match resp {
        Ok(response) => {
            let text = response.into_string().map_err(|e| e.to_string())?;
            parse_models_json(&text)
        }
        Err(ureq::Error::Status(401 | 403, _)) => Ok(UsageSnapshot {
            status: "needsAuth".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "Invalid NVIDIA API key".into(),
            backoff_until: 0,
        }),
        Err(ureq::Error::Status(429, _)) => Ok(UsageSnapshot {
            status: "backoff".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "NVIDIA rate limited (429)".into(),
            backoff_until: now_ms() + 60_000,
        }),
        Err(e) => Err(e.to_string()),
    }
}

pub fn start_updater(app: AppHandle) {
    std::thread::spawn(move || {
        let mut fail_count = 0u32;
        loop {
            let key_opt = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                find_api_key(&c.nvidia_api_key)
            };

            if let Some(key) = key_opt {
                match fetch_usage(&key) {
                    Ok(snap) => {
                        fail_count = 0;
                        persist(&snap);
                        {
                            let st = app.state::<AppState>();
                            let mut lock = st.nvidia.lock().unwrap();
                            *lock = snap.clone();
                        }
                        let _ = app.emit("nvidia", &snap);
                        crate::broadcast(&app);
                    }
                    Err(e) => {
                        fail_count = fail_count.saturating_add(1);
                        crate::applog(&format!("nvidia: fetch failed ({fail_count}): {e}"));
                    }
                }
            } else {
                let snap = UsageSnapshot {
                    status: "absent".into(),
                    windows: Vec::new(),
                    fetched_at: now_ms(),
                    note: "No NVIDIA API key configured".into(),
                    backoff_until: 0,
                };
                {
                    let st = app.state::<AppState>();
                    let mut lock = st.nvidia.lock().unwrap();
                    *lock = snap.clone();
                }
                let _ = app.emit("nvidia", &snap);
            }

            for _ in 0..POLL_SECS {
                if REFRESH.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        }
    });
}
