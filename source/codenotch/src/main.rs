#![cfg_attr(all(not(debug_assertions), windows), windows_subsystem = "windows")]

mod autostart;
mod config;
mod doctor;
mod focus;
mod hooks_install;
mod i18n;
mod server;
mod state;
mod tray;
mod usage;
mod codex;
mod cursor;
mod antigravity;
mod agy_cli;
mod glyphs;
mod trayicon;
mod activity;
mod diag;
mod watcher;
mod deepseek;
mod glm;
mod gemini_api;
mod claude_api;
mod copilot;
mod ollama;
mod nvidia;

use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

/// Logical size of the notch window: sleek ~44px pill column on the right plus room for the hover card on the left.
pub const NOTCH_W: f64 = 280.0;
pub const NOTCH_H: f64 = 480.0;
pub const NOTCH_HORIZONTAL_W: f64 = 560.0;
pub const NOTCH_HORIZONTAL_H: f64 = 340.0;
/// Hand-bumped build tag, written to run.log at startup so a log can always be matched to the exe that wrote it.
pub const BUILD: &str = "r34";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NotchPosition {
    pub edge: String,
    pub offset: f64,
}

pub struct AppState {
    pub store: Mutex<state::Store>,
    pub cfg: Mutex<config::Config>,
    pub usage: Mutex<usage::UsageSnapshot>,
    /// Codex snapshot (same UsageSnapshot shape; status may also be none/absent)
    pub codex: Mutex<usage::UsageSnapshot>,
    pub cursor: Mutex<usage::UsageSnapshot>,
    pub antigravity: Mutex<usage::UsageSnapshot>,
    pub deepseek: Mutex<usage::UsageSnapshot>,
    pub glm: Mutex<usage::UsageSnapshot>,
    pub gemini_api: Mutex<usage::UsageSnapshot>,
    pub claude_api: Mutex<usage::UsageSnapshot>,
    pub copilot: Mutex<usage::UsageSnapshot>,
    pub ollama: Mutex<usage::UsageSnapshot>,
    pub nvidia: Mutex<usage::UsageSnapshot>,
    /// Provider glyph cache, collected at launch and again on a tray refresh
    pub glyphs: Mutex<std::collections::HashMap<String, glyphs::Glyph>>,
    /// Working state of the non-Claude providers (Cursor reports it; Codex and Antigravity are inferred from recent writes)
    pub activity: Mutex<Vec<activity::Activity>>,
}

fn resolved_lang(raw: &str) -> String {
    if raw == "auto" {
        i18n::resolve_auto().to_string()
    } else {
        raw.to_string()
    }
}

/// The size multiplier chosen with the slider, clamped to what the config allows.
pub fn ui_scale(app: &AppHandle) -> f64 {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.scale.clamp(config::SCALE_MIN, config::SCALE_MAX)
}

pub fn broadcast(app: &AppHandle) {
    let st = app.state::<AppState>();
    let snap = {
        let store = st.store.lock().unwrap();
        let cfg = st.cfg.lock().unwrap();
        store.snapshot(&cfg.lang, &resolved_lang(&cfg.lang), false)
    };
    let _ = app.emit("state", &snap);
}

/// Positions the notch on any monitor edge (right, left, top, bottom) with corner/center offset.
pub fn place_notch(app: &AppHandle) {
    let Some(w) = app.get_webview_window("notch") else {
        return;
    };
    let (edge, offset) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        let e = if c.notch_edge.is_empty() { "right".to_string() } else { c.notch_edge.clone() };
        let o = if c.notch_offset > 0.0 {
            c.notch_offset
        } else if c.notch_y > 0.0 {
            c.notch_y
        } else {
            c.notch_offset
        };
        (e, o.clamp(0.0, 1.0))
    };
    let is_horizontal = edge == "top" || edge == "bottom";
    let mon = w.current_monitor().ok().flatten().or_else(|| w.primary_monitor().ok().flatten());
    let (ms, mx, my, mw, mh) = if let Some(mon) = mon {
        (
            mon.scale_factor(),
            mon.position().x,
            mon.position().y,
            mon.size().width as i32,
            mon.size().height as i32,
        )
    } else {
        (1.0, 0, 0, 1920, 1080)
    };

    let (req_w, req_h) = if is_horizontal {
        ((NOTCH_HORIZONTAL_W * ms).round() as u32, (NOTCH_HORIZONTAL_H * ms).round() as u32)
    } else {
        ((NOTCH_W * ms).round() as u32, (NOTCH_H * ms).round() as u32)
    };
    let target = tauri::PhysicalSize::new(req_w, req_h);
    let _ = w.set_size(target);

    let ww = req_w as i32;
    let wh = req_h as i32;

    let margin = (8.0 * ms).round() as i32;
    let (x, y) = match edge.as_str() {
        "left" => {
            let x = mx;
            let max_y = (mh - wh - margin * 2).max(0);
            let y = my + margin + (max_y as f64 * offset).round() as i32;
            (x, y.clamp(my + margin, (my + mh - wh - margin).max(my)))
        }
        "top" => {
            let max_x = (mw - ww - margin * 2).max(0);
            let x = mx + margin + (max_x as f64 * offset).round() as i32;
            let y = my;
            (x.clamp(mx + margin, (mx + mw - ww - margin).max(mx)), y)
        }
        "bottom" => {
            let max_x = (mw - ww - margin * 2).max(0);
            let x = mx + margin + (max_x as f64 * offset).round() as i32;
            // Leave room for the Windows taskbar (~48px at 100% scale)
            let taskbar_pad = (48.0 * ms).round() as i32;
            let y = (my + mh - wh - taskbar_pad).max(my);
            (x.clamp(mx + margin, (mx + mw - ww - margin).max(mx)), y)
        }
        _ => { // "right"
            let x = mx + mw - ww;
            let max_y = (mh - wh - margin * 2).max(0);
            let y = my + margin + (max_y as f64 * offset).round() as i32;
            (x, y.clamp(my + margin, (my + mh - wh - margin).max(my)))
        }
    };

    let pill_w = 56.0 * ms;
    let init_rect = match edge.as_str() {
        "left" => [0.0, 0.0, pill_w, req_h as f64],
        "top" => [0.0, 0.0, req_w as f64, pill_w],
        "bottom" => [0.0, req_h as f64 - pill_w, req_w as f64, pill_w],
        _ => [req_w as f64 - pill_w, 0.0, pill_w, req_h as f64],
    };
    {
        let mut h = HOT.lock().unwrap();
        if h.is_empty() {
            *h = vec![init_rect];
        }
    }

    let _ = w.set_position(tauri::PhysicalPosition::new(x, y));
    let _ = w.set_always_on_top(true);
    noactivate(app);
    let _ = app.emit("notch_position", &NotchPosition { edge: edge.clone(), offset });
    applog(&format!(
        "notch placed build={BUILD}: edge={edge} offset={offset:.3} pos=({x},{y}) size=({ww}x{wh}) monitor=({},{} {}x{})",
        mx, my, mw, mh
    ));
}

/// Reset bar to right edge center
pub fn reset_bar(app: &AppHandle) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_edge = "right".to_string();
        c.notch_offset = 0.5;
        c.notch_y = 0.5;
        config::save(&c);
    }
    let _ = app.emit("notch_position", &NotchPosition {
        edge: "right".to_string(),
        offset: 0.5,
    });
    place_notch(app);
}

/// Drag anywhere across the screen edges. Tracks cursor position in physical pixels,
/// detects the closest monitor edge (top, bottom, left, right) and position ratio along that edge,
/// snapping the notch dynamically.
static DRAGGING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
fn left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 }
}
#[cfg(not(windows))]
fn left_button_down() -> bool {
    false
}

#[tauri::command]
fn drag_begin(app: AppHandle) {
    if DRAGGING.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(move || {
        let Some(w) = app.get_webview_window("notch") else {
            DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let (Ok(start_cur), Ok(_start_pos), Ok(_size), Ok(Some(mon))) =
            (app.cursor_position(), w.outer_position(), w.outer_size(), w.primary_monitor())
        else {
            DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
            return;
        };
        let (mx, my, mw, mh) = (
            mon.position().x,
            mon.position().y,
            mon.size().width as i32,
            mon.size().height as i32,
        );
        let mut last_edge = {
            let st = app.state::<AppState>();
            let c = st.cfg.lock().unwrap();
            if c.notch_edge.is_empty() { "right".to_string() } else { c.notch_edge.clone() }
        };
        let mut last_offset = {
            let st = app.state::<AppState>();
            let c = st.cfg.lock().unwrap();
            if c.notch_offset > 0.0 { c.notch_offset } else { c.notch_y }
        };
        let mut moved = false;
        loop {
            if !left_button_down() {
                break;
            }
            if let Ok(cur) = app.cursor_position() {
                if (cur.x - start_cur.x).abs() > 4.0 || (cur.y - start_cur.y).abs() > 4.0 {
                    moved = true;
                    let cx = cur.x as i32;
                    let cy = cur.y as i32;
                    let d_top = (cy - my).max(0);
                    let d_bottom = (my + mh - cy).max(0);
                    let d_left = (cx - mx).max(0);
                    let d_right = (mx + mw - cx).max(0);

                    let min_d = d_top.min(d_bottom).min(d_left).min(d_right);
                    let cur_d = match last_edge.as_str() {
                        "top" => d_top,
                        "bottom" => d_bottom,
                        "left" => d_left,
                        _ => d_right,
                    };

                    // Edge lock: while close to the current edge (< 140 physical px), stay locked on it
                    // so dragging all the way to top/bottom corners never abruptly snaps to another edge.
                    let new_edge = if cur_d < 140 {
                        last_edge.as_str()
                    } else if min_d + 80 < cur_d {
                        if min_d == d_top {
                            "top"
                        } else if min_d == d_bottom {
                            "bottom"
                        } else if min_d == d_left {
                            "left"
                        } else {
                            "right"
                        }
                    } else {
                        last_edge.as_str()
                    };

                    let new_offset = if new_edge == "top" || new_edge == "bottom" {
                        ((cx - mx) as f64 / mw as f64).clamp(0.0, 1.0)
                    } else {
                        ((cy - my) as f64 / mh as f64).clamp(0.0, 1.0)
                    };

                    if new_edge != last_edge || (new_offset - last_offset).abs() > 0.015 {
                        last_edge = new_edge.to_string();
                        last_offset = new_offset;
                        {
                            let st = app.state::<AppState>();
                            let mut c = st.cfg.lock().unwrap();
                            c.notch_edge = last_edge.clone();
                            c.notch_offset = last_offset;
                        }
                        place_notch(&app);
                    }
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(8));
        }
        if moved {
            let st = app.state::<AppState>();
            let mut c = st.cfg.lock().unwrap();
            c.notch_edge = last_edge.clone();
            c.notch_offset = last_offset;
            c.notch_y = last_offset;
            config::save(&c);
            let _ = app.emit("notch_position", &NotchPosition {
                edge: last_edge.clone(),
                offset: last_offset,
            });
            applog(&format!("notch drag finished: edge={last_edge} offset={last_offset:.3}"));
        }
        DRAGGING.store(false, std::sync::atomic::Ordering::SeqCst);
        let _ = app.emit("drag_end", moved);
    });
}
pub fn place_bar(app: &AppHandle) {
    place_notch(app);
}
pub fn toggle_drag(app: &AppHandle) {
    // The notch stays welded to the edge; kept as a no-op for the tray menu code path
    let _ = app;
}

pub fn apply_lang(app: &AppHandle, lang: &str) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.lang = lang.to_string();
        config::save(&c);
    }
    // Through refresh_menu, which makes sure the swap happens on the main thread: doing it from the
    // settings window's thread left the tray with a menu that would never open again.
    tray::refresh_menu(app);
    broadcast(app);
}

/// The notch must never take focus: WS_EX_NOACTIVATE applied cleanly without breaking transparent DWM composition
#[cfg(windows)]
pub fn noactivate(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("notch") {
        let _ = w.set_always_on_top(true);
        if let Ok(h) = w.hwnd() {
            unsafe {
                use windows::Win32::UI::WindowsAndMessaging::{
                    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_NOACTIVATE,
                };
                let hwnd = windows::Win32::Foundation::HWND(h.0 as isize as *mut core::ffi::c_void);
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                if (ex & (WS_EX_NOACTIVATE.0 as isize)) == 0 {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | (WS_EX_NOACTIVATE.0 as isize));
                }
            }
        }
    }
}
#[cfg(not(windows))]
pub fn noactivate(_app: &AppHandle) {}

// ---------------- commands ----------------

#[tauri::command]
fn get_state(state: tauri::State<AppState>) -> state::Snapshot {
    let store = state.store.lock().unwrap();
    let cfg = state.cfg.lock().unwrap();
    store.snapshot(&cfg.lang, &resolved_lang(&cfg.lang), false)
}

#[tauri::command]
fn get_usage(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.usage.lock().unwrap().clone()
}

#[tauri::command]
fn refresh_usage(app: AppHandle) {
    {
        let st = app.state::<AppState>();
        let mut u = st.usage.lock().unwrap();
        u.backoff_until = 0;
    }
    usage::request_refresh();
    codex::request_refresh();
    cursor::request_refresh();
    antigravity::request_refresh();
    deepseek::request_refresh();
    glm::request_refresh();
    gemini_api::request_refresh();
    claude_api::request_refresh();
    copilot::request_refresh();
    ollama::request_refresh();
    nvidia::request_refresh();
}

#[tauri::command]
fn get_antigravity(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.antigravity.lock().unwrap().clone()
}

#[tauri::command]
fn get_deepseek(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.deepseek.lock().unwrap().clone()
}

#[tauri::command]
fn get_glm(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.glm.lock().unwrap().clone()
}

#[tauri::command]
fn get_gemini_api(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.gemini_api.lock().unwrap().clone()
}

#[tauri::command]
fn get_claude_api(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.claude_api.lock().unwrap().clone()
}

#[tauri::command]
fn get_copilot(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.copilot.lock().unwrap().clone()
}

#[tauri::command]
fn get_ollama(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.ollama.lock().unwrap().clone()
}

#[tauri::command]
fn get_nvidia(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.nvidia.lock().unwrap().clone()
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct ApiKeysConfig {
    pub deepseek: String,
    pub glm: String,
    pub gemini: String,
    pub gemini_budget: u64,
    pub claude: String,
    pub claude_budget: f64,
    pub copilot: String,
    pub nvidia: String,
}

#[tauri::command]
fn get_api_keys(app: AppHandle) -> ApiKeysConfig {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    ApiKeysConfig {
        deepseek: c.deepseek_api_key.clone(),
        glm: c.glm_api_key.clone(),
        gemini: c.gemini_api_key.clone(),
        gemini_budget: c.gemini_api_budget,
        claude: c.claude_api_key.clone(),
        claude_budget: c.claude_api_budget,
        copilot: c.copilot_token.clone(),
        nvidia: c.nvidia_api_key.clone(),
    }
}

#[tauri::command]
fn set_api_keys(app: AppHandle, keys: ApiKeysConfig) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.deepseek_api_key = keys.deepseek;
        c.glm_api_key = keys.glm;
        c.gemini_api_key = keys.gemini;
        c.gemini_api_budget = keys.gemini_budget;
        c.claude_api_key = keys.claude;
        c.claude_api_budget = keys.claude_budget;
        c.copilot_token = keys.copilot;
        c.nvidia_api_key = keys.nvidia;
        config::save(&c);
    }
    deepseek::request_refresh();
    glm::request_refresh();
    gemini_api::request_refresh();
    claude_api::request_refresh();
    copilot::request_refresh();
    ollama::request_refresh();
    nvidia::request_refresh();
}

#[tauri::command]
fn get_activity(state: tauri::State<AppState>) -> Vec<activity::Activity> {
    state.activity.lock().unwrap().clone()
}

#[tauri::command]
fn get_glyphs(state: tauri::State<AppState>) -> std::collections::HashMap<String, glyphs::Glyph> {
    state.glyphs.lock().unwrap().clone()
}

/// Collects the glyphs again and pushes them to the page (tray refresh, or the user just dropped in an override)
pub fn reload_glyphs(app: &AppHandle) {
    let m = glyphs::collect();
    let st = app.state::<AppState>();
    *st.glyphs.lock().unwrap() = m.clone();
    let _ = app.emit("glyphs", &m);
}

#[tauri::command]
fn open_data_dir() {
    let dir = config::config_path().parent().map(|p| p.to_path_buf()).unwrap_or_default();
    let _ = std::fs::create_dir_all(glyphs::user_dir());
    let mut cmd = std::process::Command::new("explorer");
    cmd.arg(dir.as_os_str());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.spawn();
}

#[tauri::command]
fn get_cursor(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.cursor.lock().unwrap().clone()
}

#[tauri::command]
fn get_codex(state: tauri::State<AppState>) -> usage::UsageSnapshot {
    state.codex.lock().unwrap().clone()
}

/// A click on a cell opens that provider's usage page
#[tauri::command]
fn open_provider_page(provider: String) {
    let url = match provider.as_str() {
        "codex" => "https://chatgpt.com/#settings/Account",
        "cursor" => "https://cursor.com/dashboard",
        "gemini" => "https://antigravity.google",
        "deepseek" => "https://platform.deepseek.com/usage",
        "glm" => "https://z.ai/manage-apikey/apikey-list",
        "gemini_api" => "https://aistudio.google.com/",
        "claude_api" => "https://console.anthropic.com/settings/billing",
        "copilot" => "https://github.com/settings/copilot",
        "ollama" => "http://127.0.0.1:11434",
        "nvidia" => "https://build.nvidia.com/explore/discover",
        _ => "https://claude.ai/settings/usage",
    };
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", url]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let _ = cmd.spawn();
}

/// Hot rectangles in **physical pixels**, window-relative, as x,y,w,h: the pill, plus the card
/// while it is open. The page converts by its own devicePixelRatio before reporting, so no scale
/// conversion happens here — WebView2's DPR and the window's scale_factor can disagree (see
/// report_dpr).
///
/// Empty means click-through: before the page has reported, one lost click on the notch beats
/// eating every click aimed at the window behind it.
static HOT: Mutex<Vec<[f64; 4]>> = Mutex::new(Vec::new());

/// Read only by the collapse timer — the click gate goes by the rectangles, since the pill is
/// clickable whether or not the card is up.
static EXPANDED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[tauri::command]
fn set_hot(rects: Vec<[f64; 4]>, expanded: bool) {
    *HOT.lock().unwrap() = rects;
    EXPANDED.store(expanded, std::sync::atomic::Ordering::Relaxed);
    if expanded {
        antigravity::request_hover_refresh();
    }
}

/// Setting `WS_EX_TRANSPARENT` by hand instead looks like it should work, and does not: it applies
/// to the notch window, but WebView2 keeps child HWNDs that hit-testing descends into and they
/// never get the bit. `WS_EX_LAYERED` is what makes the window answer as one surface, so the helper
/// that sets both is the only route. Clearing it again is safe — the notch is not otherwise layered
/// (its transparency is DWM composition), so the window returns to the styles it had.
fn set_click_through(app: &AppHandle, on: bool) {
    let Some(w) = app.get_webview_window("notch") else { return };
    let _ = w.set_ignore_cursor_events(on);
}

/// The WebView zoom currently applied (1.0 = uncorrected)
static ZOOM: Mutex<f64> = Mutex::new(1.0);

pub fn applog(line: &str) {
    use std::io::Write;
    let log = config::config_path().with_file_name("run.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
        let _ = writeln!(f, "{line}");
    }
}

/// Root cause: with two monitors (150 % / 200 %) WebView2 picked a devicePixelRatio of 2.0 while
/// the window was sized for the primary monitor's 1.5, so the page was 255 CSS px wide instead of
/// the designed 340 and every coordinate conversion was off (the watchdog misfired and the card
/// flashed away). Fix: the page reports its DPR, and when it differs from the primary monitor's
/// scale, set_zoom pulls the effective DPR back to that scale, restoring the 340 px width.
#[tauri::command]
fn report_dpr(app: AppHandle, dpr: f64, w: f64, h: f64) {
    let Some(win) = app.get_webview_window("notch") else { return };
    let want = win
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or_else(|| win.scale_factor().unwrap_or(1.0));
    let mut z = ZOOM.lock().unwrap();
    let base = if *z > 0.0 { dpr / *z } else { dpr };
    let target = if base > 0.0 { want / base } else { 1.0 };
    applog(&format!(
        "dpr report: dpr={dpr:.3} viewport={w:.0}x{h:.0} monitor_scale={want:.3} zoom_applied={:.3} -> target_zoom={target:.3}",
        *z
    ));
    // Oscillation guard: at most three corrections per process (if the DPR does not follow the zoom, stop chasing it)
    static APPLIED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if (dpr - want).abs() > 0.02
        && (target - *z).abs() > 0.01
        && (0.25..=4.0).contains(&target)
        && APPLIED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 3
    {
        match win.set_zoom(target) {
            Ok(()) => {
                *z = target;
                applog(&format!("dpr correction: set_zoom({target:.3}) ok"));
            }
            Err(e) => applog(&format!("dpr correction failed: {e}")),
        }
    }
}

/// Slack around every hot rectangle: this is sampled on a timer, so a cursor arriving at the pill
/// has to count as arrived slightly early, or a quick click lands between two polls while the
/// window is still click-through and goes to whatever is behind it.
const HOT_PAD: f64 = 10.0;

/// Is the cursor on something the window is there for? `window` is the outer size in physical
/// pixels, or None when it could not be read.
fn cursor_in_hot(rects: &[[f64; 4]], lx: f64, ly: f64, window: Option<(f64, f64)>) -> bool {
    if rects.is_empty() {
        return false;
    }
    let in_window = window
        .map(|(w, h)| lx >= 0.0 && ly >= 0.0 && lx < w && ly < h)
        .unwrap_or(true);
    if !in_window {
        return false;
    }
    if rects.iter().any(|r| {
        lx >= r[0] - HOT_PAD
            && ly >= r[1] - HOT_PAD
            && lx < r[0] + r[2] + HOT_PAD
            && ly < r[1] + r[3] + HOT_PAD
    }) {
        return true;
    }
    // The gap between hot rectangles (pill and card) counts as inside: use the bounding box of all of them
    if rects.len() > 1 {
        let x0 = rects.iter().map(|r| r[0]).fold(f64::MAX, f64::min);
        let y0 = rects.iter().map(|r| r[1]).fold(f64::MAX, f64::min);
        let x1 = rects.iter().map(|r| r[0] + r[2]).fold(f64::MIN, f64::max);
        let y1 = rects.iter().map(|r| r[1] + r[3]).fold(f64::MIN, f64::max);
        return lx >= x0 && ly >= y0 && lx < x1 && ly < y1;
    }
    false
}

/// Was 150 ms, when this only decided whether the card stayed up. It now also gates whether a click
/// reaches the notch, and at 150 ms a click arriving in the wrong sample went to the window behind.
const WATCHDOG_MS: u64 = 50;
/// Kept at the original 300 ms rather than falling out of the faster poll, which would make the
/// card twitchy.
const LEAVE_MS: u64 = 300;

/// WebView2's mouseleave is unreliable inside a NOACTIVATE transparent window — a cursor that
/// leaves quickly often produces no WM_MOUSELEAVE, and the card stays up. Rather than trust DOM
/// events, the Rust side watches the system cursor and emits pointer_left once it is outside; the
/// page collapses after its 250 ms grace period. "Outside the window" is not the test, though: the
/// window is mostly transparent, so the cursor is compared against the hot rectangles the page
/// reports (pill, card, and the gap between them).
///
/// It also gates click-through (#106), which is why it runs whether or not the card is open. That
/// ordering is load-bearing: the window ignores the cursor while it is click-through, so the page
/// gets no mousemove out there and cannot see the pointer arriving. This loop does, and hands the
/// window its input back in time for the page to open the card.
fn start_pointer_watchdog(app: AppHandle) {
    std::thread::spawn(move || {
        let need = (LEAVE_MS / WATCHDOG_MS).max(1) as u8;
        let mut miss = 0u8;
        // Last value pushed: this changes only when the cursor crosses an edge
        let mut click_through: Option<bool> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(WATCHDOG_MS));
            let Some(w) = app.get_webview_window("notch") else { continue };
            let (Ok(pos), Ok(cur)) = (w.outer_position(), app.cursor_position()) else { continue };
            let rects = HOT.lock().unwrap().clone();
            if rects.is_empty() {
                continue;
            }
            // Cursor position relative to the window's top-left, in physical pixels; the hot rectangles are physical too, so no scale conversion
            let lx = cur.x - pos.x as f64;
            let ly = cur.y - pos.y as f64;
            let size = w.outer_size().ok().map(|s| (s.width as f64, s.height as f64));
            let inside = cursor_in_hot(&rects, lx, ly, size);

            if click_through != Some(!inside) {
                set_click_through(&app, !inside);
                click_through = Some(!inside);
                applog(&format!(
                    "click-through {} at cursor_rel=({lx:.0},{ly:.0}) rects={rects:?}",
                    if inside { "off (cursor on the notch)" } else { "on (cursor elsewhere)" }
                ));
            }

            static LOGGED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 12 {
                applog(&format!(
                    "watchdog: cursor_rel=({lx:.0},{ly:.0}) inside={inside} rects={rects:?} winpos=({},{})",
                    pos.x, pos.y
                ));
            }

            if !EXPANDED.load(std::sync::atomic::Ordering::Relaxed) {
                miss = 0;
                continue;
            }
            if inside {
                miss = 0;
            } else {
                miss += 1;
                if miss >= need {
                    miss = 0;
                    EXPANDED.store(false, std::sync::atomic::Ordering::Relaxed);
                    let _ = app.emit("pointer_left", ());
                }
            }
        }
    });
}

/// Log channel for the page: JS writes key diagnostics into run.log (if invoke itself fails, the page reports on screen instead)
#[tauri::command]
fn log_js(msg: String) {
    applog(&format!("js: {}", msg.chars().take(600).collect::<String>()));
}

#[tauri::command]
fn open_usage_page() {
    let mut cmd = std::process::Command::new("cmd");
    cmd.args(["/C", "start", "", "https://claude.ai/settings/usage"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let _ = cmd.spawn();
}

#[tauri::command]
fn focus_session(app: AppHandle, id: String) -> bool {
    let ppid = {
        let st = app.state::<AppState>();
        let store = st.store.lock().unwrap();
        store.ppid_of(&id)
    };
    match ppid {
        Some(p) => focus::focus_terminal(p),
        None => focus::focus_claude_desktop(),
    }
}

#[tauri::command]
fn dismiss_session(app: AppHandle, id: String) {
    {
        let st = app.state::<AppState>();
        let mut store = st.store.lock().unwrap();
        store.dismiss(&id);
    }
    broadcast(&app);
}

#[tauri::command]
fn set_lang(app: AppHandle, lang: String) {
    apply_lang(&app, &lang);
}

// ---------------- notch size ----------------

#[tauri::command]
fn get_scale(app: AppHandle) -> f64 {
    ui_scale(&app)
}

/// Called by the slider on every move. Only the value is stored here: the page scales the pill
/// itself with a CSS zoom, so the window is never resized and the hover card holding the slider
/// keeps its size — otherwise the slider would shrink away from under the cursor mid-drag.
#[tauri::command]
fn set_scale(app: AppHandle, scale: f64) {
    let value = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.scale = scale.clamp(config::SCALE_MIN, config::SCALE_MAX);
        config::save(&c);
        c.scale
    };
    // The notch draws its own size, so it has to be told. Without this the slider in the settings
    // window saved the value but nothing changed on screen until the app was restarted.
    let _ = app.emit("scale", value);
}

// ---------------- tray icon readings ----------------

/// The headline percentage of a reading: its fullest window, as a whole number 0-100.
///
/// Deliberately the same rule as `headlineOf` in ui/notch.html, so the tray icon and the ring can
/// never disagree: consider only metered windows — a `count` window (Antigravity's requests today,
/// say) has no published denominator, so its `used` is not a share of anything and averaging or
/// maximising over it would invent a number.
fn headline_pct(s: &usage::UsageSnapshot) -> Option<u32> {
    let top = s
        .windows
        .iter()
        .filter(|w| w.count.is_none())
        .map(|w| w.used)
        .fold(f64::NAN, f64::max);
    if top.is_nan() {
        return None;
    }
    Some((top * 100.0).round().clamp(0.0, 100.0) as u32)
}

/// Ids match the ones the page uses, so the tray, the settings window and the notch all agree.
fn snapshot_of(app: &AppHandle, id: &str) -> usage::UsageSnapshot {
    let st = app.state::<AppState>();
    match id {
        "codex" => st.codex.lock().unwrap().clone(),
        "cursor" => st.cursor.lock().unwrap().clone(),
        "gemini" => st.antigravity.lock().unwrap().clone(),
        "deepseek" => st.deepseek.lock().unwrap().clone(),
        "glm" => st.glm.lock().unwrap().clone(),
        "gemini_api" => st.gemini_api.lock().unwrap().clone(),
        "claude_api" => st.claude_api.lock().unwrap().clone(),
        "copilot" => st.copilot.lock().unwrap().clone(),
        "ollama" => st.ollama.lock().unwrap().clone(),
        "nvidia" => st.nvidia.lock().unwrap().clone(),
        _ => st.usage.lock().unwrap().clone(),
    }
}

/// What one half of the icon should show. An empty (or "top") window id means "whichever of this
/// provider's windows is fullest", which is the notch's own rule and the only choice that survives
/// a provider adding or renaming its windows.
fn reading_for_slot(app: &AppHandle, slot: &config::TraySlot) -> Option<u32> {
    let snap = snapshot_of(app, &slot.provider);
    if snap.status == "absent" {
        return None;
    }
    let is_rem = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        c.display_mode != "used"
    };
    let val = if slot.window.is_empty() || slot.window == "top" {
        headline_pct(&snap)
    } else {
        // A pinned window missing from this snapshot falls back to the fullest one, which is what the
        // notch does in the same situation. Without this a provider that renamed or dropped a window
        // would leave the icon showing a dash while the notch still showed a number.
        snap.windows
            .iter()
            .find(|w| w.id == slot.window && w.count.is_none())
            .map(|w| (w.used * 100.0).round().clamp(0.0, 100.0) as u32)
            .or_else(|| headline_pct(&snap))
    };
    if is_rem {
        val.map(|v| 100u32.saturating_sub(v))
    } else {
        val
    }
}

/// One provider and the windows it currently reports, for the settings window's pickers. Built
/// from live readings rather than a hard-coded table, so a provider that gains a window shows it.
#[derive(serde::Serialize)]
struct TrayOption {
    id: String,
    label: String,
    status: String,
    windows: Vec<TrayWindowOption>,
}

#[derive(serde::Serialize)]
struct TrayWindowOption {
    id: String,
    label: String,
    used: Option<u32>,
}

#[tauri::command]
fn get_tray_options(app: AppHandle) -> Vec<TrayOption> {
    TRAY_PROVIDER_IDS
        .iter()
        .map(|id| {
            let snap = snapshot_of(&app, id);
            TrayOption {
                id: (*id).to_string(),
                label: provider_label(id).to_string(),
                status: snap.status.clone(),
                windows: snap
                    .windows
                    .iter()
                    .filter(|w| w.count.is_none()) // a count window has no percentage to draw
                    .map(|w| TrayWindowOption {
                        id: w.id.clone(),
                        label: w.label.clone(),
                        used: Some((w.used * 100.0).round().clamp(0.0, 100.0) as u32),
                    })
                    .collect(),
            }
        })
        .collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TrayConfig {
    mode: String,
    slots: Vec<config::TraySlot>,
}

#[tauri::command]
fn get_tray_config(app: AppHandle) -> TrayConfig {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    TrayConfig { mode: c.tray_mode.clone(), slots: c.tray_slots.clone() }
}

#[tauri::command]
fn set_tray_config(app: AppHandle, cfg: TrayConfig) {
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.tray_mode = cfg.mode;
        c.tray_slots = cfg.slots;
        // Kept in step so an older build reading this file still shows something sensible
        c.tray_providers = c.tray_slots.iter().map(|s| s.provider.clone()).collect();
        config::save(&c);
    }
    repaint_tray(&app);
    tray::refresh_menu(&app);
}

/// The real icon, as a picture, for the settings preview — so what is being edited cannot drift
/// from what the taskbar actually draws.
#[tauri::command]
fn get_tray_preview(app: AppHandle, cfg: TrayConfig) -> Option<String> {
    let values: Vec<Option<u32>> = cfg.slots.iter().map(|s| reading_for_slot(&app, s)).collect();
    let rgba = match cfg.mode.as_str() {
        "bars" if !values.is_empty() => trayicon::bars_rgba(&values),
        "numbers" if !values.is_empty() => trayicon::numbers_rgba(&values),
        _ => return None, // "off" shows the app's own mark, which the page draws itself
    };
    trayicon::to_data_url(&rgba)
}

/// What each ring on the notch shows: the provider, and which of its windows. An empty list means
/// every provider, each showing whichever window is fullest — the original behaviour.
#[tauri::command]
fn get_notch_slots(app: AppHandle) -> Vec<config::TraySlot> {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.notch_slots.clone()
}

#[tauri::command]
fn set_notch_slots(app: AppHandle, slots: Vec<config::TraySlot>) {
    let list = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_slots = slots;
        // Kept in step so an older build reading this file still shows the right providers
        c.notch_providers = c.notch_slots.iter().map(|s| s.provider.clone()).collect();
        config::save(&c);
        c.notch_slots.clone()
    };
    // The notch is a separate window and draws its own cells, so it has to be told.
    let _ = app.emit("notch_slots", list);
}

/// The application's own icon, so the settings window shows what the taskbar shows.
#[tauri::command]
fn get_app_icon() -> Option<String> {
    trayicon::app_mark_data_url()
}

// ---------------- what is on screen at all ----------------

#[derive(serde::Serialize)]
struct UiFlags {
    notch_visible: bool,
    tray_visible: bool,
}

#[tauri::command]
fn get_ui_flags(app: AppHandle) -> UiFlags {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    UiFlags { notch_visible: c.notch_visible, tray_visible: c.tray_visible }
}

/// Hiding both would leave the app running with nothing to click, so the tray icon is kept
/// whenever the notch is off. The answer says what was actually stored, so the settings window can
/// show the corrected state rather than a lie.
#[tauri::command]
fn set_ui_flags(app: AppHandle, notch_visible: bool, tray_visible: bool) -> UiFlags {
    let flags = {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_visible = notch_visible;
        c.tray_visible = if notch_visible { tray_visible } else { true };
        config::save(&c);
        UiFlags { notch_visible: c.notch_visible, tray_visible: c.tray_visible }
    };
    apply_visibility(&app);
    flags
}

/// Puts the two switches into effect.
pub fn apply_visibility(app: &AppHandle) {
    let (notch, tray_on) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.notch_visible, c.tray_visible)
    };
    if let Some(w) = app.get_webview_window("notch") {
        if notch {
            let _ = w.show();
            place_notch(app);
        } else {
            let _ = w.hide();
        }
    }
    if let Some(t) = app.tray_by_id("main") {
        let _ = t.set_visible(tray_on);
    }
}

// ---------------- settings that used to live in the tray menu ----------------

#[tauri::command]
fn get_lang(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    c.lang.clone()
}

/// The settings WebView must use the same Windows locale as the tray. WebView2's
/// navigator.language can describe the browser runtime rather than the user locale.
#[tauri::command]
fn get_lang_resolved(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    resolved_lang(&c.lang)
}

#[tauri::command]
fn get_autostart() -> bool {
    autostart::is_enabled()
}

#[tauri::command]
fn set_autostart(on: bool) -> Result<String, String> {
    if on {
        autostart::enable()
    } else {
        autostart::disable()
    }
}

#[tauri::command]
fn get_hooks_installed() -> bool {
    hooks_install::is_installed()
}

#[tauri::command]
fn set_hooks_installed(on: bool) -> Result<String, String> {
    if on {
        hooks_install::install()
    } else {
        hooks_install::uninstall()
    }
}

#[tauri::command]
fn reset_notch_position(app: AppHandle) {
    reset_bar(&app);
}

#[tauri::command]
fn get_notch_position(app: AppHandle) -> NotchPosition {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    NotchPosition {
        edge: if c.notch_edge.is_empty() { "right".to_string() } else { c.notch_edge.clone() },
        offset: if c.notch_offset > 0.0 { c.notch_offset.clamp(0.0, 1.0) } else { c.notch_y.clamp(0.0, 1.0) },
    }
}

#[tauri::command]
fn set_notch_position(app: AppHandle, edge: String, offset: f64) -> Result<NotchPosition, String> {
    let valid_edge = match edge.as_str() {
        "left" => "left",
        "top" => "top",
        "bottom" => "bottom",
        _ => "right",
    };
    let valid_offset = offset.clamp(0.0, 1.0);
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.notch_edge = valid_edge.to_string();
        c.notch_offset = valid_offset;
        c.notch_y = valid_offset;
        config::save(&c);
    }
    let pos = NotchPosition {
        edge: valid_edge.to_string(),
        offset: valid_offset,
    };
    let _ = app.emit("notch_position", &pos);
    place_notch(&app);
    Ok(pos)
}

#[tauri::command]
fn open_settings(app: AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[tauri::command]
fn get_display_mode(app: AppHandle) -> String {
    let st = app.state::<AppState>();
    let c = st.cfg.lock().unwrap();
    if c.display_mode.is_empty() {
        "remaining".to_string()
    } else {
        c.display_mode.clone()
    }
}

#[tauri::command]
fn set_display_mode(app: AppHandle, mode: String) -> Result<String, String> {
    let valid = if mode == "used" { "used" } else { "remaining" };
    {
        let st = app.state::<AppState>();
        let mut c = st.cfg.lock().unwrap();
        c.display_mode = valid.to_string();
        config::save(&c);
    }
    let _ = app.emit("display_mode", valid);
    repaint_tray(&app);
    Ok(valid.to_string())
}

pub fn provider_label(id: &str) -> &'static str {
    match id {
        "codex" => "Codex",
        "cursor" => "Cursor",
        "gemini" => "Antigravity",
        "deepseek" => "DeepSeek",
        "glm" => "GLM",
        "gemini_api" => "Gemini API",
        "claude_api" => "Claude API",
        "copilot" => "GitHub Copilot",
        "ollama" => "Ollama",
        "nvidia" => "NVIDIA NIM",
        _ => "Claude",
    }
}

/// Every provider the tray menu can offer, in the order the notch shows them.
pub const TRAY_PROVIDER_IDS: [&str; 11] = [
    "claude", "codex", "cursor", "gemini",
    "deepseek", "glm", "gemini_api", "claude_api", "copilot", "ollama", "nvidia",
];

/// Draws the icon and writes the tooltip. Shared by the polling thread and by the settings window,
/// so a change made in settings shows up at once rather than on the next poll.
fn paint_tray(app: &AppHandle, mode: &str, slots: &[config::TraySlot], values: &[Option<u32>]) {
    let handle = app.clone();
    let mode = mode.to_string();
    let values = values.to_vec();
    let slots = slots.to_vec();
    let _ = app.run_on_main_thread(move || {
        let Some(tray) = handle.tray_by_id("main") else {
            return;
        };
        let outcome = match mode.as_str() {
            "numbers" if !values.is_empty() => tray.set_icon(Some(trayicon::numbers(&values))),
            "bars" if !values.is_empty() => tray.set_icon(Some(trayicon::bars(&values))),
            // "Plain icon": the application's own icon was already set at startup
            _ => Ok(()),
        };
        if let Err(e) = outcome {
            applog(&format!("tray: set_icon FAILED mode={mode} values={values:?}: {e}"));
        }
        // The tooltip lists every slot, including any the digit layout could not fit, so nothing is
        // silently dropped.
        let parts: Vec<String> = slots
            .iter()
            .zip(values.iter())
            .map(|(slot, v)| {
                format!(
                    "{} {}",
                    provider_label(&slot.provider),
                    v.map(|p| format!("{p}%")).unwrap_or_else(|| "—".into())
                )
            })
            .collect();
        let tip = if parts.is_empty() {
            concat!("Codenotch v", env!("CARGO_PKG_VERSION")).to_string()
        } else {
            format!("Codenotch — {}", parts.join(" · "))
        };
        let _ = tray.set_tooltip(Some(&tip));
    });
}

/// Reads the current settings and readings, and repaints immediately.
pub fn repaint_tray(app: &AppHandle) {
    let (mode, slots) = {
        let st = app.state::<AppState>();
        let c = st.cfg.lock().unwrap();
        (c.tray_mode.clone(), c.tray_slots.clone())
    };
    let values: Vec<Option<u32>> = slots.iter().map(|s| reading_for_slot(app, s)).collect();
    paint_tray(app, &mode, &slots, &values);
}

/// Repaints when a reading changes. Every 2 seconds, but it only touches the icon when something
/// actually moved, so it costs nothing while idle.
fn start_tray_updater(app: AppHandle) {
    std::thread::spawn(move || {
        let mut last: Option<(String, Vec<config::TraySlot>, Vec<Option<u32>>)> = None;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let (mode, slots) = {
                let st = app.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                (c.tray_mode.clone(), c.tray_slots.clone())
            };
            let values: Vec<Option<u32>> = slots.iter().map(|s| reading_for_slot(&app, s)).collect();
            let key = (mode.clone(), slots.clone(), values.clone());
            if last.as_ref() == Some(&key) {
                continue;
            }
            last = Some(key);
            paint_tray(&app, &mode, &slots, &values);
        }
    });
}

/// Seen-clears-it: looking at a session acknowledges it (engine behaviour, unchanged)
#[cfg(windows)]
fn ack_scan(app: &AppHandle) -> bool {
    let need = {
        let st = app.state::<AppState>();
        let store = st.store.lock().unwrap();
        store.has_done()
    };
    if !need {
        return false;
    }
    let fg = focus::fg_pid();
    if fg == 0 {
        return false;
    }
    let maps = focus::proc_maps();
    let fg_name = maps.name.get(&fg).cloned().unwrap_or_default();
    let fg_is_claude_desktop = fg_name.contains("claude") && !fg_name.contains("codenotch");
    let st = app.state::<AppState>();
    let mut store = st.store.lock().unwrap();
    store.ack_done(|s| {
        if s.ppid == 0 {
            fg_is_claude_desktop
        } else {
            focus::pid_hits_chain(fg, &focus::chain_of(s.ppid, &maps.ppid), &maps)
        }
    })
}
#[cfg(not(windows))]
fn ack_scan(_app: &AppHandle) -> bool {
    false
}

// ---------------- main ----------------

#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_ok() {
            use std::fs::OpenOptions;
            use std::os::windows::io::IntoRawHandle;
            use windows::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
            if let Ok(conout) = OpenOptions::new().write(true).open("CONOUT$") {
                let handle = windows::Win32::Foundation::HANDLE(conout.into_raw_handle() as _);
                let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
                let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
            }
        }
    }
}
#[cfg(not(windows))]
fn attach_console() {}

fn report(r: Result<String, String>) {
    let msg = match r {
        Ok(m) => format!("OK: {m}"),
        Err(e) => format!("FAILED: {e}"),
    };
    println!("{msg}");
    let log = config::config_path().with_file_name("install.log");
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(log, &msg);
}

fn main() {
    attach_console();
    let args: Vec<String> = std::env::args().collect();
    if let Some(cmd) = args.get(1) {
        match cmd.as_str() {
            "install-hooks" => {
                report(hooks_install::install());
                return;
            }
            "uninstall-hooks" => {
                report(hooks_install::uninstall());
                return;
            }
            "autostart" => {
                let r = match args.get(2).map(|s| s.as_str()) {
                    Some("on") => autostart::enable(),
                    Some("off") => autostart::disable(),
                    _ => Err("usage: codenotch.exe autostart on|off".into()),
                };
                report(r);
                return;
            }
            "doctor" => {
                let out = if args.get(2).map(|s| s.as_str()) == Some("deep") { diag::run() } else { doctor::run() };
                println!("{out}");
                let log = config::config_path().with_file_name("doctor.log");
                if let Some(parent) = log.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(log, &out);
                return;
            }
            _ => {}
        }
    }

    let start_with_settings = args.iter().any(|a| a == "settings" || a == "--settings" || a == "config");
    let cfg = config::load();
    let port = cfg.port;

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            applog(&format!("single instance: another launch detected (args={args:?})"));
            if let Some(w) = app.get_webview_window("notch") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_always_on_top(true);
                place_notch(app);
                noactivate(app);
            }
            if let Some(w) = app.get_webview_window("settings") {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_focus();
            }
        }))
        .manage(AppState {
            store: Mutex::new(Default::default()),
            cfg: Mutex::new(cfg),
            usage: Mutex::new(usage::load_persisted()),
            codex: Mutex::new(codex::load_persisted()),
            cursor: Mutex::new(cursor::load_persisted()),
            antigravity: Mutex::new(antigravity::load_persisted()),
            deepseek: Mutex::new(deepseek::load_persisted()),
            glm: Mutex::new(glm::load_persisted()),
            gemini_api: Mutex::new(gemini_api::load_persisted()),
            claude_api: Mutex::new(claude_api::load_persisted()),
            copilot: Mutex::new(copilot::load_persisted()),
            ollama: Mutex::new(ollama::load_persisted()),
            nvidia: Mutex::new(nvidia::load_persisted()),
            glyphs: Mutex::new(Default::default()),
            activity: Mutex::new(Vec::new()),
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_usage,
            get_codex,
            get_cursor,
            get_antigravity,
            get_deepseek,
            get_glm,
            get_gemini_api,
            get_claude_api,
            get_copilot,
            get_ollama,
            get_nvidia,
            get_api_keys,
            set_api_keys,
            get_glyphs,
            get_activity,
            open_data_dir,
            drag_begin,
            open_provider_page,
            refresh_usage,
            open_usage_page,
            set_hot,
            report_dpr,
            log_js,
            focus_session,
            dismiss_session,
            set_lang,
            get_scale,
            set_scale,
            get_tray_options,
            get_tray_config,
            set_tray_config,
            get_tray_preview,
            get_notch_slots,
            set_notch_slots,
            get_app_icon,
            get_ui_flags,
            set_ui_flags,
            get_lang,
            get_lang_resolved,
            get_autostart,
            set_autostart,
            get_hooks_installed,
            set_hooks_installed,
            reset_notch_position,
            get_notch_position,
            set_notch_position,
            open_settings,
            get_display_mode,
            set_display_mode
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            place_notch(&handle);
            noactivate(&handle);
            if let Some(w) = handle.get_webview_window("notch") {
                let hide_me = w.clone();
                w.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = hide_me.show();
                    }
                });
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_always_on_top(true);
            }
            tray::setup(&handle)?;
            // Closing a Tauri window destroys it by default, and a destroyed window cannot be shown
            // again — which is why Settings opened once and then never again. Hide it instead.
            if let Some(w) = handle.get_webview_window("settings") {
                let hide_me = w.clone();
                w.on_window_event(move |e| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = e {
                        api.prevent_close();
                        let _ = hide_me.hide();
                    }
                });
                if start_with_settings {
                    let _ = w.show();
                    let _ = w.unminimize();
                    let _ = w.set_focus();
                }
            }
            start_tray_updater(handle.clone());
            // Honours the saved switches: a notch hidden last time stays hidden.
            apply_visibility(&handle);
            server::start(handle.clone(), port);
            watcher::start(handle.clone());
            usage::start(handle.clone());
            codex::start(handle.clone());
            cursor::start(handle.clone());
            antigravity::start(handle.clone());
            deepseek::start_updater(handle.clone());
            glm::start_updater(handle.clone());
            gemini_api::start_updater(handle.clone());
            claude_api::start_updater(handle.clone());
            copilot::start_updater(handle.clone());
            ollama::start_updater(handle.clone());
            nvidia::start_updater(handle.clone());
            activity::start(handle.clone());
            // Collecting glyphs may read icon resources out of a few executables; do it off the main thread and push when done
            let gh = handle.clone();
            std::thread::spawn(move || reload_glyphs(&gh));
            start_pointer_watchdog(handle.clone());
            // Seen-clears-it scan
            let acker = handle.clone();
            std::thread::spawn(move || {
                activity::lower_thread_priority();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    if ack_scan(&acker) {
                        broadcast(&acker);
                    }
                }
            });
            // Stale session cleanup
            let sweeper = handle.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(30));
                let changed = {
                    let st = sweeper.state::<AppState>();
                    let mut s = st.store.lock().unwrap();
                    s.sweep()
                };
                if changed {
                    broadcast(&sweeper);
                }
            });
            // Persist the config (codenotch-hook reads the port from it)
            {
                let st = handle.state::<AppState>();
                let c = st.cfg.lock().unwrap();
                config::save(&c);
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("Codenotch failed to start");
}

#[cfg(test)]
mod tests {
    use super::{cursor_in_hot, HOT_PAD};

    /// Real values from the run.log in #106: a 2560×1600 display at 150 %.
    const PILL: [f64; 4] = [405.0, 183.5, 105.0, 323.0];
    const CARD: [f64; 4] = [21.0, 142.5, 369.0, 262.0];
    const WINDOW: Option<(f64, f64)> = Some((510.0, 690.0));

    #[test]
    fn nothing_is_hot_before_the_page_reports() {
        assert!(!cursor_in_hot(&[], 450.0, 300.0, WINDOW));
    }

    #[test]
    fn the_pill_is_hot() {
        assert!(cursor_in_hot(&[PILL], 450.0, 300.0, WINDOW));
    }

    #[test]
    fn the_transparent_area_beside_the_pill_is_not() {
        assert!(!cursor_in_hot(&[PILL], 0.0, 297.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], 100.0, 400.0, WINDOW));
    }

    #[test]
    fn the_card_is_hot_while_it_is_open() {
        assert!(!cursor_in_hot(&[PILL], 100.0, 250.0, WINDOW));
        assert!(cursor_in_hot(&[PILL, CARD], 100.0, 250.0, WINDOW));
    }

    #[test]
    fn the_shipped_pill_and_card_have_no_cold_strip_between_them() {
        // The 15 px gap is narrower than the 20 px the two pads bring, so the pads already bridge
        // it and the bounding box never fires for the shipped layout. Pinned: if that stops being
        // true the crossing starts depending on the bounding box, and the card blinks out mid-travel.
        let x = (CARD[0] + CARD[2] + PILL[0]) / 2.0;
        assert!(cursor_in_hot(&[PILL, CARD], x, 250.0, WINDOW));
        const { assert!(PILL[0] - (CARD[0] + CARD[2]) < 2.0 * HOT_PAD) };
    }

    /// Far enough apart that the pads do not meet — the case the bounding box exists for.
    const FAR_A: [f64; 4] = [0.0, 0.0, 50.0, 50.0];
    const FAR_B: [f64; 4] = [200.0, 0.0, 50.0, 50.0];

    #[test]
    fn a_wide_gap_is_bridged_by_the_bounding_box() {
        assert!(cursor_in_hot(&[FAR_A, FAR_B], 125.0, 25.0, None));
    }

    #[test]
    fn the_bounding_box_needs_two_rectangles_to_bridge_anything() {
        assert!(!cursor_in_hot(&[FAR_A], 125.0, 25.0, None));
    }

    #[test]
    fn the_pad_reaches_slightly_past_the_pill() {
        assert!(cursor_in_hot(&[PILL], PILL[0] - HOT_PAD + 1.0, 300.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], PILL[0] - HOT_PAD - 1.0, 300.0, WINDOW));
    }

    #[test]
    fn a_cursor_off_the_window_is_never_hot() {
        assert!(!cursor_in_hot(&[PILL], 515.0, 300.0, WINDOW));
        assert!(!cursor_in_hot(&[PILL], 450.0, -5.0, WINDOW));
    }

    #[test]
    fn an_unreadable_window_size_falls_back_to_the_rectangles() {
        assert!(cursor_in_hot(&[PILL], 450.0, 300.0, None));
        assert!(!cursor_in_hot(&[PILL], 100.0, 300.0, None));
    }
}
