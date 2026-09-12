//! Ollama local runtime adapter.
//! Endpoint: GET http://127.0.0.1:11434/api/ps
//! Inspects loaded models and VRAM usage.

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "http://127.0.0.1:11434/api/ps";
const LM_STUDIO_ENDPOINT: &str = "http://127.0.0.1:1234/v1/models";
const POLL_SECS: u64 = 60;

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
    crate::config::config_path().with_file_name("ollama.json")
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

#[derive(Debug, Deserialize)]
struct ModelItem {
    name: String,
    size: Option<u64>,
    size_vram: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PsResponse {
    #[serde(default)]
    models: Vec<ModelItem>,
}

#[derive(Debug, Deserialize)]
struct LmStudioModel {
    id: String,
}

#[derive(Debug, Deserialize)]
struct LmStudioResponse {
    #[serde(default)]
    data: Vec<LmStudioModel>,
}

pub fn parse_ps_json(json: &str) -> Result<UsageSnapshot, String> {
    let resp: PsResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if resp.models.is_empty() {
        return Ok(UsageSnapshot {
            status: "ok".into(),
            windows: vec![LimitWindow {
                id: "idle".into(),
                label: "Ollama (Idle)".into(),
                used: 0.0,
                resets_at: None,
                count: None,
                derived: false,
            }],
            fetched_at: now_ms(),
            note: "Ollama running · 0 active models".into(),
            backoff_until: 0,
        });
    }

    let mut windows = Vec::new();
    let mut names = Vec::new();
    for m in &resp.models {
        let vram_gb = m.size_vram.unwrap_or(m.size.unwrap_or(0)) as f64 / 1_073_741_824.0;
        let short_name = m.name.split(':').next().unwrap_or(&m.name);
        names.push(short_name);
        windows.push(LimitWindow {
            id: m.name.clone(),
            label: format!("{short_name} ({vram_gb:.1}GB)"),
            used: 1.0,
            resets_at: None,
            count: None,
            derived: false,
        });
    }

    let note = format!("Ollama · {}", names.join(", "));
    Ok(UsageSnapshot {
        status: "ok".into(),
        windows,
        fetched_at: now_ms(),
        note,
        backoff_until: 0,
    })
}

pub fn parse_lm_studio_json(json: &str) -> Result<UsageSnapshot, String> {
    let resp: LmStudioResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if resp.data.is_empty() {
        return Ok(UsageSnapshot {
            status: "ok".into(),
            windows: vec![LimitWindow {
                id: "idle".into(),
                label: "LM Studio (Idle)".into(),
                used: 0.0,
                resets_at: None,
                count: None,
                derived: false,
            }],
            fetched_at: now_ms(),
            note: "LM Studio running · 0 active models".into(),
            backoff_until: 0,
        });
    }

    let mut windows = Vec::new();
    let mut names = Vec::new();
    for m in &resp.data {
        let short_name = m.id.split('/').last().unwrap_or(&m.id);
        names.push(short_name);
        windows.push(LimitWindow {
            id: m.id.clone(),
            label: format!("LM Studio: {short_name}"),
            used: 1.0,
            resets_at: None,
            count: None,
            derived: false,
        });
    }

    let note = format!("LM Studio · {}", names.join(", "));
    Ok(UsageSnapshot {
        status: "ok".into(),
        windows,
        fetched_at: now_ms(),
        note,
        backoff_until: 0,
    })
}

pub fn probe_ollama() -> Result<UsageSnapshot, String> {
    let agent = ureq::builder()
        .timeout(Duration::from_secs(2))
        .build();

    // 1. Try Ollama
    if let Ok(response) = agent.get(ENDPOINT).set("Accept", "application/json").call() {
        if let Ok(text) = response.into_string() {
            if let Ok(snap) = parse_ps_json(&text) {
                return Ok(snap);
            }
        }
    }

    // 2. Try LM Studio
    if let Ok(response) = agent.get(LM_STUDIO_ENDPOINT).set("Accept", "application/json").call() {
        if let Ok(text) = response.into_string() {
            if let Ok(snap) = parse_lm_studio_json(&text) {
                return Ok(snap);
            }
        }
    }

    Ok(UsageSnapshot {
        status: "absent".into(),
        windows: Vec::new(),
        fetched_at: now_ms(),
        note: "Local LLM (Ollama / LM Studio) not running".into(),
        backoff_until: 0,
    })
}

pub fn start_updater(app: AppHandle) {
    std::thread::spawn(move || {
        loop {
            if let Ok(snap) = probe_ollama() {
                persist(&snap);
                {
                    let st = app.state::<AppState>();
                    let mut lock = st.ollama.lock().unwrap();
                    *lock = snap.clone();
                }
                let _ = app.emit("ollama", &snap);
                crate::broadcast(&app);
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
