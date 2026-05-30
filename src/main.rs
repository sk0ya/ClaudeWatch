#![windows_subsystem = "windows"]

mod visibility;
mod platform;
mod rate_limit;
mod codex;
mod instances;
mod app;
use app::ClaudeWatchApp;

use eframe::egui;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use visibility::{VisibilityControl, start_visibility_thread, SHOW_AFTER_IDLE};
use crate::rate_limit::{RateLimitState, UsageResponse, fetch_rate_limit};
use crate::codex::{CodexRateLimitState, fetch_codex_rate_limits};
use crate::instances::{ClaudeInstance, detect_claude_instances, has_activity_since};
use crate::platform::user_idle_duration;

// --- Stats Cache (local file) ---

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct StatsCache {
    #[serde(default)]
    daily_activity: Vec<DailyActivity>,
    #[serde(default)]
    model_usage: HashMap<String, ModelUsage>,
    #[serde(default)]
    total_sessions: u64,
    #[serde(default)]
    total_messages: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct DailyActivity {
    date: String,
    #[serde(default)]
    messages: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelUsage {
    #[serde(default)]
    tokens: u64,
    #[serde(default)]
    messages: u64,
}

fn load_icon() -> egui::IconData {
    let bytes = include_bytes!("../assets/ClaudeWatch.ico");
    let img = image::load_from_memory(bytes)
        .expect("Failed to decode icon")
        .into_rgba8();
    egui::IconData {
        rgba: img.to_vec(),
        width: img.width(),
        height: img.height(),
    }
}

fn main() -> eframe::Result<()> {
    let rate_limit: Arc<Mutex<Option<RateLimitState>>> = Arc::new(Mutex::new(None));
    let codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>> = Arc::new(Mutex::new(None));
    let instances: Arc<Mutex<Vec<ClaudeInstance>>> = Arc::new(Mutex::new(Vec::new()));

    // Background thread for Claude rate limit polling
    let rl_clone = Arc::clone(&rate_limit);
    std::thread::spawn(move || {
        let mut last_good_usage: Option<UsageResponse> = None;
        let mut last_fetch_time: Option<std::time::SystemTime> = None;
        loop {
            // Skip fetch if no Claude Code activity since last fetch (and we have cached data)
            let should_fetch =
                last_good_usage.is_none() || last_fetch_time.map_or(true, has_activity_since);
            if should_fetch {
                let state = fetch_rate_limit();
                last_fetch_time = Some(std::time::SystemTime::now());
                if state.error.is_none() {
                    last_good_usage = Some(state.usage.clone());
                    *rl_clone.lock().unwrap() = Some(state);
                } else if let Some(ref cached) = last_good_usage {
                    // On error, show the last known good value silently
                    *rl_clone.lock().unwrap() = Some(RateLimitState {
                        usage: cached.clone(),
                        fetched_at: state.fetched_at,
                        error: None,
                    });
                } else {
                    *rl_clone.lock().unwrap() = Some(state);
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(300));
        }
    });

    // Background thread for Codex rate limit polling (reads local session files)
    let codex_clone = Arc::clone(&codex_rate_limit);
    std::thread::spawn(move || loop {
        let state = fetch_codex_rate_limits();
        *codex_clone.lock().unwrap() = Some(state);
        std::thread::sleep(std::time::Duration::from_secs(30));
    });

    // Background thread for instance detection
    let inst_clone = Arc::clone(&instances);
    std::thread::spawn(move || loop {
        let detected = detect_claude_instances();
        *inst_clone.lock().unwrap() = detected;
        std::thread::sleep(std::time::Duration::from_secs(5));
    });

    let rl_for_app = Arc::clone(&rate_limit);
    let codex_rl_for_app = Arc::clone(&codex_rate_limit);
    let inst_for_app = Arc::clone(&instances);
    let initial_visible = user_idle_duration().map_or(true, |idle| idle >= SHOW_AFTER_IDLE);
    let visibility_control = Arc::new(VisibilityControl::default());
    start_visibility_thread(Arc::clone(&visibility_control), initial_visible);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([300.0, 260.0])
            .with_always_on_top()
            .with_title("ClaudeWatch")
            .with_decorations(false)
            .with_taskbar(true)
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "ClaudeWatch",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(ClaudeWatchApp::new(
                rl_for_app,
                codex_rl_for_app,
                inst_for_app,
                visibility_control,
            )))
        }),
    )
}
