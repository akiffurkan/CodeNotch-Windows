//! GitHub Copilot usage adapter.
//! Endpoint: GET https://api.github.com/copilot_internal/user
//! Reads quota snapshots (premium_interactions, chat, completions).

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT: &str = "https://api.github.com/copilot_internal/user";
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
    crate::config::config_path().with_file_name("copilot.json")
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

pub fn find_token(cfg_token: &str) -> Option<String> {
    let trimmed = cfg_token.trim();
    if !trimmed.is_empty() {
        return Some(trimmed.to_string());
    }

    if let Ok(tok) = std::env::var("GH_TOKEN").or_else(|_| std::env::var("GITHUB_TOKEN")) {
        let trimmed = tok.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    // Windows GitHub CLI hosts.yml: %APPDATA%\GitHub CLI\hosts.yml
    let mut candidate_paths = Vec::new();
    if let Some(appdata) = dirs::config_dir() {
        candidate_paths.push(appdata.join("GitHub CLI").join("hosts.yml"));
        candidate_paths.push(appdata.join("gh").join("hosts.yml"));
    }
    if let Some(home) = dirs::home_dir() {
        candidate_paths.push(home.join(".config").join("gh").join("hosts.yml"));
    }

    for p in candidate_paths {
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Some(tok) = parse_hosts_yml(&content) {
                return Some(tok);
            }
        }
    }

    // Fallback: gh auth token
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["auth", "token", "--hostname", "github.com"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    if let Ok(output) = cmd.output() {
        if output.status.success() {
            let tok = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !tok.is_empty() {
                return Some(tok);
            }
        }
    }

    None
}

fn parse_hosts_yml(text: &str) -> Option<String> {
    let mut inside_github = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "github.com:" {
            inside_github = true;
            continue;
        }
        if inside_github {
            if !line.starts_with(' ') && !line.starts_with('\t') && trimmed.ends_with(':') {
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("oauth_token:") {
                let token = rest.trim().trim_matches('"').trim_matches('\'');
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct QuotaEntry {
    #[serde(default)]
    entitlement: Option<f64>,
    #[serde(default)]
    remaining: Option<f64>,
    #[serde(default)]
    used: Option<f64>,
    #[serde(default)]
    unlimited: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct CopilotResponse {
    #[serde(default)]
    copilot_plan: Option<String>,
    #[serde(default)]
    quota_snapshots: Option<HashMap<String, QuotaEntry>>,
}

pub fn parse_copilot_json(json: &str) -> Result<UsageSnapshot, String> {
    let resp: CopilotResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let quotas = resp.quota_snapshots.unwrap_or_default();
    let mut windows = Vec::new();

    let order = ["premium_interactions", "chat", "completions"];
    for &key in &order {
        if let Some(quota) = quotas.get(key) {
            if quota.unlimited == Some(true) {
                continue;
            }
            let entitlement = quota.entitlement.unwrap_or(0.0);
            if entitlement <= 0.0 {
                continue;
            }
            let used = quota.used.unwrap_or_else(|| {
                (entitlement - quota.remaining.unwrap_or(entitlement)).max(0.0)
            });
            let used_fraction = (used / entitlement).clamp(0.0, 1.0);

            let label = match key {
                "premium_interactions" => "Premium Requests",
                "chat" => "Copilot Chat",
                "completions" => "Completions",
                _ => key,
            };

            windows.push(LimitWindow {
                id: key.to_string(),
                label: label.to_string(),
                used: used_fraction,
                resets_at: None,
                count: None,
                derived: false,
            });
        }
    }

    let plan = resp.copilot_plan.unwrap_or_else(|| "Copilot".into());
    let note = format!("GitHub {plan}");

    Ok(UsageSnapshot {
        status: "ok".into(),
        windows,
        fetched_at: now_ms(),
        note,
        backoff_until: 0,
    })
}

pub fn fetch_usage(token: &str) -> Result<UsageSnapshot, String> {
    let agent = ureq::builder()
        .timeout(Duration::from_secs(15))
        .build();

    let resp = agent
        .get(ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .set("X-GitHub-Api-Version", "2022-11-28")
        .set("User-Agent", "Codenotch")
        .call();

    match resp {
        Ok(response) => {
            let text = response.into_string().map_err(|e| e.to_string())?;
            parse_copilot_json(&text)
        }
        Err(ureq::Error::Status(401 | 403, _)) => Ok(UsageSnapshot {
            status: "needsAuth".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "GitHub Copilot auth required (run `gh auth login`)".into(),
            backoff_until: 0,
        }),
        Err(ureq::Error::Status(429, _)) => Ok(UsageSnapshot {
            status: "backoff".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "GitHub Copilot rate limited (429)".into(),
            backoff_until: now_ms() + 60_000,
        }),
        Err(e) => Err(e.to_string()),
    }
}

pub fn start_updater(app: AppHandle) {
    std::thread::spawn(move || {
        let mut fail_count = 0u32;
        loop {
            let token_opt = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                find_token(&c.copilot_token)
            };

            if let Some(token) = token_opt {
                match fetch_usage(&token) {
                    Ok(snap) => {
                        fail_count = 0;
                        persist(&snap);
                        {
                            let st = app.state::<AppState>();
                            let mut lock = st.copilot.lock().unwrap();
                            *lock = snap.clone();
                        }
                        let _ = app.emit("copilot", &snap);
                        crate::broadcast(&app);
                    }
                    Err(e) => {
                        fail_count = fail_count.saturating_add(1);
                        crate::applog(&format!("copilot: fetch failed ({fail_count}): {e}"));
                    }
                }
            } else {
                let snap = UsageSnapshot {
                    status: "absent".into(),
                    windows: Vec::new(),
                    fetched_at: now_ms(),
                    note: "No GitHub Copilot token found".into(),
                    backoff_until: 0,
                };
                {
                    let st = app.state::<AppState>();
                    let mut lock = st.copilot.lock().unwrap();
                    *lock = snap.clone();
                }
                let _ = app.emit("copilot", &snap);
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
    fn test_parse_hosts() {
        let yml = "github.com:\n    user: testuser\n    oauth_token: gho_1234567890\n";
        assert_eq!(parse_hosts_yml(yml), Some("gho_1234567890".into()));
    }

    #[test]
    fn test_parse_copilot_json() {
        let json = r#"{
            "copilot_plan": "individual",
            "quota_snapshots": {
                "premium_interactions": {
                    "entitlement": 300,
                    "remaining": 210,
                    "used": 90
                }
            }
        }"#;
        let snap = parse_copilot_json(json).expect("should parse");
        assert_eq!(snap.status, "ok");
        assert_eq!(snap.windows.len(), 1);
        let w = &snap.windows[0];
        assert_eq!(w.id, "premium_interactions");
        assert!((w.used - 0.3).abs() < 0.001);
    }
}
