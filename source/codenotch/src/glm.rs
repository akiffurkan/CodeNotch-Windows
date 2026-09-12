//! GLM / Z.ai Coding Plan usage adapter.
//! Endpoint: GET https://api.z.ai/api/monitor/usage/quota/limit (or open.bigmodel.cn)
//! Header: Authorization: Bearer <token>
//!
//! Windows:
//!   - session: 5-hour rolling session (unit=3, number=5)
//!   - weekly: weekly limit (unit=6, number=1)
//!   - mcp: monthly MCP time limit (type="TIME_LIMIT")

use crate::usage::{LimitWindow, UsageSnapshot};
use crate::AppState;
use serde::Deserialize;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const ENDPOINT_ZAI: &str = "https://api.z.ai/api/monitor/usage/quota/limit";
const ENDPOINT_BIGMODEL: &str = "https://open.bigmodel.cn/api/monitor/usage/quota/limit";
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
    crate::config::config_path().with_file_name("glm.json")
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
struct LimitEntry {
    #[serde(rename = "type")]
    limit_type: Option<String>,
    unit: Option<i32>,
    number: Option<i32>,
    percentage: Option<f64>,
    #[serde(rename = "nextResetTime")]
    next_reset_time: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GlmData {
    level: Option<String>,
    #[serde(default)]
    limits: Vec<LimitEntry>,
}

#[derive(Debug, Deserialize)]
struct GlmResponse {
    code: Option<i32>,
    success: Option<bool>,
    data: Option<GlmData>,
    msg: Option<String>,
}

pub struct GlmCreds {
    pub token: String,
    pub endpoint: &'static str,
}

pub fn find_credentials(cfg_key: &str) -> Option<GlmCreds> {
    let trimmed = cfg_key.trim();
    if !trimmed.is_empty() {
        let endpoint = if trimmed.contains("bigmodel") {
            ENDPOINT_BIGMODEL
        } else {
            ENDPOINT_ZAI
        };
        return Some(GlmCreds {
            token: trimmed.to_string(),
            endpoint,
        });
    }

    if let Ok(env_key) = std::env::var("GLM_API_KEY").or_else(|_| std::env::var("ZAI_API_KEY")) {
        let env_trimmed = env_key.trim();
        if !env_trimmed.is_empty() {
            return Some(GlmCreds {
                token: env_trimmed.to_string(),
                endpoint: ENDPOINT_ZAI,
            });
        }
    }

    // Claude Code settings.json
    if let Some(home) = dirs::home_dir() {
        let claude_settings = home.join(".claude").join("settings.json");
        if let Ok(content) = std::fs::read_to_string(&claude_settings) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(env) = v.get("env") {
                    let base_url = env.get("ANTHROPIC_BASE_URL").and_then(|u| u.as_str()).unwrap_or("");
                    if base_url.contains("z.ai") || base_url.contains("bigmodel.cn") {
                        if let Some(tok) = env.get("ANTHROPIC_AUTH_TOKEN").or_else(|| env.get("ANTHROPIC_API_KEY")).and_then(|t| t.as_str()) {
                            let endpoint = if base_url.contains("bigmodel.cn") {
                                ENDPOINT_BIGMODEL
                            } else {
                                ENDPOINT_ZAI
                            };
                            return Some(GlmCreds {
                                token: tok.trim().to_string(),
                                endpoint,
                            });
                        }
                    }
                }
            }
        }

        // ZCode config: ~/.zcode/v2/config.json
        let zcode_cfg = home.join(".zcode").join("v2").join("config.json");
        if let Ok(content) = std::fs::read_to_string(&zcode_cfg) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(providers) = v.get("provider").and_then(|p| p.as_object()) {
                    for (id, val) in providers {
                        if id.contains("coding-plan") {
                            let enabled = val.get("enabled").and_then(|b| b.as_bool()).unwrap_or(true);
                            if enabled {
                                if let Some(key) = val.get("options").and_then(|o| o.get("apiKey")).and_then(|k| k.as_str()) {
                                    let base = val.get("options").and_then(|o| o.get("baseURL")).and_then(|u| u.as_str()).unwrap_or("");
                                    let endpoint = if base.contains("bigmodel.cn") {
                                        ENDPOINT_BIGMODEL
                                    } else {
                                        ENDPOINT_ZAI
                                    };
                                    return Some(GlmCreds {
                                        token: key.trim().to_string(),
                                        endpoint,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    None
}

pub fn parse_glm_json(json: &str) -> Result<UsageSnapshot, String> {
    let resp: GlmResponse = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let ok = resp.success.unwrap_or(false) || resp.code == Some(200);
    if !ok {
        let code = resp.code.unwrap_or(0);
        if code == 401 || code == 403 {
            return Ok(UsageSnapshot {
                status: "needsAuth".into(),
                windows: Vec::new(),
                fetched_at: now_ms(),
                note: "GLM / Z.ai credentials invalid or expired".into(),
                backoff_until: 0,
            });
        }
        return Err(format!("GLM error: code={code} msg={:?}", resp.msg));
    }

    let data = resp.data.ok_or("No data object in GLM response")?;
    let mut windows = Vec::new();

    for limit in data.limits {
        let pct = match limit.percentage {
            Some(p) => p / 100.0,
            None => continue,
        };

        if limit.limit_type.as_deref() == Some("TIME_LIMIT") {
            windows.push(LimitWindow {
                id: "mcp".into(),
                label: "MCP (1 month)".into(),
                used: pct.clamp(0.0, 1.0),
                resets_at: limit.next_reset_time,
                count: None,
                derived: false,
            });
        } else {
            match (limit.unit, limit.number) {
                (Some(3), Some(5)) => {
                    windows.push(LimitWindow {
                        id: "session".into(),
                        label: "Current session (5h)".into(),
                        used: pct.clamp(0.0, 1.0),
                        resets_at: limit.next_reset_time,
                        count: None,
                        derived: false,
                    });
                }
                (Some(6), Some(1)) => {
                    windows.push(LimitWindow {
                        id: "weekly".into(),
                        label: "Weekly allowance".into(),
                        used: pct.clamp(0.0, 1.0),
                        resets_at: limit.next_reset_time,
                        count: None,
                        derived: false,
                    });
                }
                _ => {
                    let id = format!("limit_{}_{}", limit.unit.unwrap_or(0), limit.number.unwrap_or(0));
                    windows.push(LimitWindow {
                        id,
                        label: "Quota".into(),
                        used: pct.clamp(0.0, 1.0),
                        resets_at: limit.next_reset_time,
                        count: None,
                        derived: false,
                    });
                }
            }
        }
    }

    // Sort: session first, then weekly, then mcp
    windows.sort_by_key(|w| match w.id.as_str() {
        "session" => 0,
        "weekly" => 1,
        "mcp" => 2,
        _ => 3,
    });

    let plan_name = data.level.unwrap_or_else(|| "Plan".into());
    let note = format!("GLM {plan_name}");

    Ok(UsageSnapshot {
        status: "ok".into(),
        windows,
        fetched_at: now_ms(),
        note,
        backoff_until: 0,
    })
}

pub fn fetch_usage(creds: &GlmCreds) -> Result<UsageSnapshot, String> {
    let agent = ureq::builder()
        .timeout(Duration::from_secs(15))
        .build();

    let resp = agent
        .get(creds.endpoint)
        .set("Authorization", &format!("Bearer {}", creds.token))
        .set("Accept", "application/json")
        .call();

    match resp {
        Ok(response) => {
            let text = response.into_string().map_err(|e| e.to_string())?;
            parse_glm_json(&text)
        }
        Err(ureq::Error::Status(401 | 403, _)) => Ok(UsageSnapshot {
            status: "needsAuth".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "GLM authorization failed".into(),
            backoff_until: 0,
        }),
        Err(ureq::Error::Status(429, _)) => Ok(UsageSnapshot {
            status: "backoff".into(),
            windows: Vec::new(),
            fetched_at: now_ms(),
            note: "GLM rate limited (429)".into(),
            backoff_until: now_ms() + 60_000,
        }),
        Err(e) => Err(e.to_string()),
    }
}

pub fn start_updater(app: AppHandle) {
    std::thread::spawn(move || {
        let mut fail_count = 0u32;
        loop {
            let creds_opt = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                find_credentials(&c.glm_api_key)
            };

            if let Some(creds) = creds_opt {
                match fetch_usage(&creds) {
                    Ok(snap) => {
                        fail_count = 0;
                        persist(&snap);
                        {
                            let st = app.state::<AppState>();
                            let mut lock = st.glm.lock().unwrap();
                            *lock = snap.clone();
                        }
                        let _ = app.emit("glm", &snap);
                        crate::broadcast(&app);
                    }
                    Err(e) => {
                        fail_count = fail_count.saturating_add(1);
                        crate::applog(&format!("glm: fetch failed ({fail_count}): {e}"));
                    }
                }
            } else {
                let snap = UsageSnapshot {
                    status: "absent".into(),
                    windows: Vec::new(),
                    fetched_at: now_ms(),
                    note: "No GLM key configured".into(),
                    backoff_until: 0,
                };
                {
                    let st = app.state::<AppState>();
                    let mut lock = st.glm.lock().unwrap();
                    *lock = snap.clone();
                }
                let _ = app.emit("glm", &snap);
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
    fn test_parse_glm_response() {
        let json = r#"{
            "code": 200,
            "success": true,
            "data": {
                "level": "pro",
                "limits": [
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 3,
                        "number": 5,
                        "percentage": 14.5,
                        "nextResetTime": 1788682200000
                    },
                    {
                        "type": "TOKENS_LIMIT",
                        "unit": 6,
                        "number": 1,
                        "percentage": 9.2,
                        "nextResetTime": 1789190400000
                    },
                    {
                        "type": "TIME_LIMIT",
                        "percentage": 5.0
                    }
                ]
            }
        }"#;

        let snap = parse_glm_json(json).expect("should parse");
        assert_eq!(snap.status, "ok");
        assert_eq!(snap.windows.len(), 3);
        assert_eq!(snap.windows[0].id, "session");
        assert!((snap.windows[0].used - 0.145).abs() < 0.001);
        assert_eq!(snap.windows[0].resets_at, Some(1788682200000));
        assert_eq!(snap.windows[1].id, "weekly");
        assert_eq!(snap.windows[2].id, "mcp");
    }
}
