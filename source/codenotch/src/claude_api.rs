//! Claude API usage adapter (Anthropic API Key).
//! For users with direct ANTHROPIC_API_KEY, independent of Claude Code CLI OAuth.
//! Endpoint: GET https://api.anthropic.com/v1/models
//! Header: x-api-key: <key>
//! Header: anthropic-version: 2023-06-01

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://api.anthropic.com/v1/models";
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
    crate::config::config_path().with_file_name("claude_api.json")
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
    if let Ok(env_key) = std::env::var("ANTHROPIC_API_KEY") {
        let env_trimmed = env_key.trim();
        if !env_trimmed.is_empty() {
            return Some(env_trimmed.to_string());
        }
    }
    None
}

pub fn validate_and_snapshot(key: &str, _budget: f64) -> Result<UsageSnapshot, String> {
    let agent = ureq::builder()
        .timeout(Duration::from_secs(15))
        .build();

    let resp = agent
        .get(ENDPOINT)
        .set("x-api-key", key)
        .set("anthropic-version", "2023-06-01")
        .set("Accept", "application/json")
        .call();

    match resp {
        Ok(response) => {
            let text = response.into_string().map_err(|e| e.to_string())?;
            let model_count = if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                val.get("data").and_then(|d| d.as_array()).map(|a| a.len()).unwrap_or(0)
            } else {
                0
            };

            let window = LimitWindow {
                id: "api".into(),
                label: "Claude API Key".into(),
                used: 0.0,
                resets_at: None,
                count: None,
                derived: false,
            };

            Ok(UsageSnapshot {
                status: "ok".into(),
                windows: vec![window],
                fetched_at: now_ms(),
                note: format!("Claude API Active · {model_count} models"),
                backoff_until: 0,
            })
        }
        Err(ureq::Error::Status(401 | 403, _)) => Ok(UsageSnapshot {
            status: "needsAuth".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "Invalid Anthropic API key".into(),
            backoff_until: 0,
        }),
        Err(ureq::Error::Status(429, _)) => Ok(UsageSnapshot {
            status: "backoff".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "Anthropic API rate limited (429)".into(),
            backoff_until: now_ms() + 60_000,
        }),
        Err(e) => Err(e.to_string()),
    }
}

pub fn start_updater(app: AppHandle) {
    std::thread::spawn(move || {
        let mut fail_count = 0u32;
        loop {
            let (key_opt, budget) = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                (find_api_key(&c.claude_api_key), c.claude_api_budget)
            };

            if let Some(key) = key_opt {
                match validate_and_snapshot(&key, budget) {
                    Ok(snap) => {
                        fail_count = 0;
                        persist(&snap);
                        {
                            let st = app.state::<AppState>();
                            let mut lock = st.claude_api.lock().unwrap();
                            *lock = snap.clone();
                        }
                        let _ = app.emit("claude_api", &snap);
                        crate::broadcast(&app);
                    }
                    Err(e) => {
                        fail_count = fail_count.saturating_add(1);
                        crate::applog(&format!("claude_api: fetch failed ({fail_count}): {e}"));
                    }
                }
            } else {
                let snap = UsageSnapshot {
                    status: "absent".into(),
                    windows: Vec::new(),
                    fetched_at: now_ms(),
                    note: "No Claude API key configured".into(),
                    backoff_until: 0,
                };
                {
                    let st = app.state::<AppState>();
                    let mut lock = st.claude_api.lock().unwrap();
                    *lock = snap.clone();
                }
                let _ = app.emit("claude_api", &snap);
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
