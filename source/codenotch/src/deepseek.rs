//! DeepSeek usage adapter.
//! Endpoint: GET https://api.deepseek.com/user/balance
//! Header: Authorization: Bearer <token>
//!
//! Parses balance_infos: [{ currency, total_balance, granted_balance, topped_up_balance }]
//! Computes used fraction = max(0, topped_up_balance + granted_balance - total_balance) / max(topped_up_balance + granted_balance, total_balance).

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://api.deepseek.com/user/balance";
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
    crate::config::config_path().with_file_name("deepseek.json")
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
struct BalanceInfo {
    currency: String,
    total_balance: String,
    #[serde(default)]
    granted_balance: String,
    #[serde(default)]
    topped_up_balance: String,
}

#[derive(Debug, Deserialize)]
struct BalanceResponse {
    is_available: bool,
    #[serde(default)]
    balance_infos: Vec<BalanceInfo>,
}

/// Discovers DeepSeek API key from:
/// 1. config.deepseek_api_key
/// 2. DEEPSEEK_API_KEY environment variable
/// 3. ~/.deepseek/auth.json or %APPDATA%\deepseek\auth.json
pub fn find_api_key(cfg_key: &str) -> Option<String> {
    let trimmed = cfg_key.trim();
    if !trimmed.is_empty() {
        return Some(trimmed.to_string());
    }
    if let Ok(env_key) = std::env::var("DEEPSEEK_API_KEY") {
        let env_trimmed = env_key.trim();
        if !env_trimmed.is_empty() {
            return Some(env_trimmed.to_string());
        }
    }
    // Check ~/.deepseek/auth.json
    if let Some(home) = dirs::home_dir() {
        let p = home.join(".deepseek").join("auth.json");
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(k) = val.get("apiKey").or_else(|| val.get("api_key")).and_then(|v| v.as_str()) {
                    let k_trimmed = k.trim();
                    if !k_trimmed.is_empty() {
                        return Some(k_trimmed.to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn parse_balance_json(json: &str) -> Result<UsageSnapshot, String> {
    let resp: BalanceResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if !resp.is_available {
        return Ok(UsageSnapshot {
            status: "needsAuth".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "DeepSeek account unavailable or disabled".into(),
            backoff_until: 0,
        });
    }
    let info = resp.balance_infos.first().ok_or("No balance info found")?;
    let total: f64 = info.total_balance.parse().unwrap_or(0.0);
    let granted: f64 = info.granted_balance.parse().unwrap_or(0.0);
    let topped_up: f64 = info.topped_up_balance.parse().unwrap_or(0.0);

    let symbol = match info.currency.as_str() {
        "CNY" => "¥",
        "USD" => "$",
        _ => info.currency.as_str(),
    };

    let funded = (topped_up + granted).max(total);
    let used_fraction = if funded > 0.0 {
        let spent = (funded - total).max(0.0);
        (spent / funded).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let label = format!("Balance ({symbol})");
    let note = format!("{symbol}{total:.2} balance · {symbol}{funded:.2} total funded");

    let window = LimitWindow {
        id: "balance".into(),
        label,
        used: used_fraction,
        resets_at: None,
        count: None,
        derived: false,
    };

    Ok(UsageSnapshot {
        status: "ok".into(),
        windows: vec![window],
        fetched_at: now_ms(),
        note,
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
            parse_balance_json(&text)
        }
        Err(ureq::Error::Status(401 | 403, _)) => Ok(UsageSnapshot {
            status: "needsAuth".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "Invalid DeepSeek API key".into(),
            backoff_until: 0,
        }),
        Err(ureq::Error::Status(429, _)) => Ok(UsageSnapshot {
            status: "backoff".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "DeepSeek rate limited (429)".into(),
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
                find_api_key(&c.deepseek_api_key)
            };

            if let Some(key) = key_opt {
                match fetch_usage(&key) {
                    Ok(snap) => {
                        fail_count = 0;
                        persist(&snap);
                        {
                            let st = app.state::<AppState>();
                            let mut lock = st.deepseek.lock().unwrap();
                            *lock = snap.clone();
                        }
                        let _ = app.emit("deepseek", &snap);
                        crate::broadcast(&app);
                    }
                    Err(e) => {
                        fail_count = fail_count.saturating_add(1);
                        crate::applog(&format!("deepseek: fetch failed ({fail_count}): {e}"));
                    }
                }
            } else {
                // Not configured
                let snap = UsageSnapshot {
                    status: "absent".into(),
                    windows: Vec::new(),
                    fetched_at: now_ms(),
                    note: "No DeepSeek API key configured".into(),
                    backoff_until: 0,
                };
                {
                    let st = app.state::<AppState>();
                    let mut lock = st.deepseek.lock().unwrap();
                    *lock = snap.clone();
                }
                let _ = app.emit("deepseek", &snap);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_balance_cny() {
        let json = r#"{
            "is_available": true,
            "balance_infos": [
                {
                    "currency": "CNY",
                    "total_balance": "8.50",
                    "granted_balance": "0.00",
                    "topped_up_balance": "10.00"
                }
            ]
        }"#;
        let snap = parse_balance_json(json).expect("should parse");
        assert_eq!(snap.status, "ok");
        assert_eq!(snap.windows.len(), 1);
        let w = &snap.windows[0];
        assert_eq!(w.id, "balance");
        assert_eq!(w.label, "Balance (¥)");
        // spent = 10 - 8.5 = 1.5, used = 1.5 / 10 = 0.15
        assert!((w.used - 0.15).abs() < 0.001);
    }

    #[test]
    fn test_parse_balance_unavailable() {
        let json = r#"{
            "is_available": false,
            "balance_infos": []
        }"#;
        let snap = parse_balance_json(json).expect("should parse");
        assert_eq!(snap.status, "needsAuth");
    }
}
