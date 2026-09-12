//! Google Gemini API usage adapter (Google AI Studio API Key).
//! Distinct from Antigravity (which meters the Google Antigravity IDE/CLI).
//! Endpoint: GET https://generativelanguage.googleapis.com/v1beta/models?key=<key>
//! Also reads token counts from local Gemini CLI sessions (~/.gemini/tmp/).

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const MODELS_ENDPOINT: &str = "https://generativelanguage.googleapis.com/v1beta/models";
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
    crate::config::config_path().with_file_name("gemini_api.json")
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
    if let Ok(env_key) = std::env::var("GEMINI_API_KEY") {
        let env_trimmed = env_key.trim();
        if !env_trimmed.is_empty() {
            return Some(env_trimmed.to_string());
        }
    }
    None
}

/// Reads token count from Gemini CLI sessions on disk (~/.gemini/tmp/)
pub fn count_cli_tokens() -> u64 {
    let mut total: u64 = 0;
    let Some(home) = dirs::home_dir() else { return 0 };
    let tmp_dir = home.join(".gemini").join("tmp");
    if let Ok(entries) = std::fs::read_dir(tmp_dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                let chats_dir = p.join("chats");
                if let Ok(chat_files) = std::fs::read_dir(chats_dir) {
                    for f in chat_files.flatten() {
                        if f.path().extension().and_then(|s| s.to_str()) == Some("json") {
                            if let Ok(content) = std::fs::read_to_string(f.path()) {
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                                    if let Some(tokens) = val.get("tokens").or_else(|| val.get("tokenCount")).and_then(|t| t.as_u64()) {
                                        total = total.saturating_add(tokens);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    total
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ModelItem {
    name: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    models: Vec<ModelItem>,
}

pub fn validate_and_snapshot(key: &str, budget: u64) -> Result<UsageSnapshot, String> {
    let agent = ureq::builder()
        .timeout(Duration::from_secs(15))
        .build();

    let url = format!("{MODELS_ENDPOINT}?key={key}");
    let resp = agent.get(&url).set("Accept", "application/json").call();

    match resp {
        Ok(response) => {
            let text = response.into_string().map_err(|e| e.to_string())?;
            let models_resp: ModelsResponse = serde_json::from_str(&text).unwrap_or(ModelsResponse { models: Vec::new() });
            let model_count = models_resp.models.len();

            let cli_tokens = count_cli_tokens();
            let effective_budget = if budget > 0 { budget } else { 1_000_000 };
            let used_fraction = (cli_tokens as f64 / effective_budget as f64).clamp(0.0, 1.0);

            let mut windows = Vec::new();
            windows.push(LimitWindow {
                id: "monthly".into(),
                label: "Token Budget".into(),
                used: used_fraction,
                resets_at: None,
                count: None,
                derived: false,
            });

            let note = format!("Gemini API · {model_count} models active · {cli_tokens} tokens");

            Ok(UsageSnapshot {
                status: "ok".into(),
                windows,
                fetched_at: now_ms(),
                note,
                backoff_until: 0,
            })
        }
        Err(ureq::Error::Status(400 | 403, _)) => Ok(UsageSnapshot {
            status: "needsAuth".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "Invalid Google Gemini API key".into(),
            backoff_until: 0,
        }),
        Err(ureq::Error::Status(429, _)) => Ok(UsageSnapshot {
            status: "backoff".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "Gemini API rate limited".into(),
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
                (find_api_key(&c.gemini_api_key), c.gemini_api_budget)
            };

            if let Some(key) = key_opt {
                match validate_and_snapshot(&key, budget) {
                    Ok(snap) => {
                        fail_count = 0;
                        persist(&snap);
                        {
                            let st = app.state::<AppState>();
                            let mut lock = st.gemini_api.lock().unwrap();
                            *lock = snap.clone();
                        }
                        let _ = app.emit("gemini_api", &snap);
                        crate::broadcast(&app);
                    }
                    Err(e) => {
                        fail_count = fail_count.saturating_add(1);
                        crate::applog(&format!("gemini_api: fetch failed ({fail_count}): {e}"));
                    }
                }
            } else {
                let snap = UsageSnapshot {
                    status: "absent".into(),
                    windows: Vec::new(),
                    fetched_at: now_ms(),
                    note: "No Gemini API key configured".into(),
                    backoff_until: 0,
                };
                {
                    let st = app.state::<AppState>();
                    let mut lock = st.gemini_api.lock().unwrap();
                    *lock = snap.clone();
                }
                let _ = app.emit("gemini_api", &snap);
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
