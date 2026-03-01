#![windows_subsystem = "windows"]

use eframe::egui;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

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
    message_count: u64,
    #[serde(default)]
    session_count: u64,
    #[serde(default)]
    tool_call_count: u64,
}

#[derive(Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
struct ModelUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

// --- Rate Limit API ---

#[derive(Deserialize, Clone, Debug)]
struct Credentials {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<OAuthInfo>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
struct OAuthInfo {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct UsageResponse {
    five_hour: Option<RateLimitEntry>,
    seven_day: Option<RateLimitEntry>,
    seven_day_opus: Option<RateLimitEntry>,
    seven_day_sonnet: Option<RateLimitEntry>,
    extra_usage: Option<ExtraUsageEntry>,
}

#[derive(Deserialize, Clone, Debug)]
struct RateLimitEntry {
    utilization: f64,
    resets_at: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
struct ExtraUsageEntry {
    is_enabled: bool,
    monthly_limit: Option<f64>,
    used_credits: Option<f64>,
    utilization: Option<f64>,
}

#[derive(Clone, Debug)]
struct RateLimitState {
    usage: UsageResponse,
    fetched_at: chrono::DateTime<chrono::Local>,
    error: Option<String>,
}

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const BETA_HEADER: &str = "oauth-2025-04-20";

fn creds_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join(".credentials.json"))
}

fn read_credentials() -> Result<OAuthInfo, String> {
    let path = creds_path().ok_or("Home directory not found")?;
    let data = std::fs::read_to_string(&path).map_err(|e| format!("Read creds: {e}"))?;
    let creds: Credentials =
        serde_json::from_str(&data).map_err(|e| format!("Parse creds: {e}"))?;
    creds
        .claude_ai_oauth
        .ok_or_else(|| "No OAuth credentials".into())
}

fn refresh_token(refresh_tok: &str) -> Result<OAuthInfo, String> {
    let client = reqwest::blocking::Client::new();
    let body = serde_json::json!({
        "grant_type": "refresh_token",
        "refresh_token": refresh_tok,
        "client_id": CLIENT_ID,
    });
    let resp = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .map_err(|e| format!("Refresh request: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("Refresh failed: {}", resp.status()));
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: u64,
    }

    let tok: TokenResponse = resp.json().map_err(|e| format!("Parse token: {e}"))?;
    let expires_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
        + tok.expires_in * 1000;

    // Update credentials file
    if let Some(path) = creds_path() {
        let new_creds = serde_json::json!({
            "claudeAiOauth": {
                "accessToken": tok.access_token,
                "refreshToken": tok.refresh_token,
                "expiresAt": expires_at,
                "scopes": ["user:inference", "user:mcp_servers", "user:profile", "user:sessions:claude_code"],
                "subscriptionType": "pro",
                "rateLimitTier": "default_claude_ai"
            }
        });
        let _ = std::fs::write(path, serde_json::to_string(&new_creds).unwrap());
    }

    Ok(OAuthInfo {
        access_token: tok.access_token,
        refresh_token: tok.refresh_token,
        expires_at,
    })
}

fn fetch_usage(access_token: &str) -> Result<UsageResponse, String> {
    let client = reqwest::blocking::Client::new();
    let resp = client
        .get(USAGE_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("anthropic-beta", BETA_HEADER)
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .map_err(|e| format!("Usage request: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        return Err(format!("Usage API {status}: {body}"));
    }

    resp.json::<UsageResponse>()
        .map_err(|e| format!("Parse usage: {e}"))
}

fn fetch_rate_limit() -> RateLimitState {
    let now = chrono::Local::now();

    let result = (|| -> Result<UsageResponse, String> {
        let mut oauth = read_credentials()?;

        // Refresh if expiring within 5 minutes
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        if now_ms + 300_000 >= oauth.expires_at {
            oauth = refresh_token(&oauth.refresh_token)?;
        }

        fetch_usage(&oauth.access_token)
    })();

    match result {
        Ok(usage) => RateLimitState {
            usage,
            fetched_at: now,
            error: None,
        },
        Err(e) => RateLimitState {
            usage: UsageResponse::default(),
            fetched_at: now,
            error: Some(e),
        },
    }
}

// --- Codex Rate Limit (from local session files) ---

#[derive(Clone, Debug)]
struct CodexWindowInfo {
    used_percent: f64,
    resets_at: u64,    // Unix timestamp (seconds)
    window_minutes: u64,
}

#[derive(Clone, Debug)]
struct CodexRateLimit {
    limit_id: String,
    limit_name: Option<String>,
    primary: CodexWindowInfo,
    secondary: CodexWindowInfo,
}

#[derive(Clone, Debug, Default)]
struct CodexRateLimitState {
    limits: Vec<CodexRateLimit>,
    read_at: chrono::DateTime<chrono::Local>,
    error: Option<String>,
}

fn fetch_codex_rate_limits() -> CodexRateLimitState {
    let now = chrono::Local::now();

    let result = (|| -> Result<Vec<CodexRateLimit>, String> {
        let codex_dir = dirs::home_dir()
            .ok_or_else(|| "No home dir".to_string())?
            .join(".codex")
            .join("sessions");

        if !codex_dir.exists() {
            return Err("Codex not installed".into());
        }

        // Collect all JSONL session files with modification times
        let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> = Vec::new();
        let Ok(year_iter) = std::fs::read_dir(&codex_dir) else {
            return Err("Cannot read sessions".into());
        };
        for ye in year_iter.flatten() {
            let Ok(mi) = std::fs::read_dir(ye.path()) else { continue };
            for me in mi.flatten() {
                let Ok(di) = std::fs::read_dir(me.path()) else { continue };
                for de in di.flatten() {
                    // de = day directory; read files inside it
                    let Ok(fi) = std::fs::read_dir(de.path()) else { continue };
                    for fe in fi.flatten() {
                        let p = fe.path();
                        if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            if let Ok(meta) = p.metadata() {
                                if let Ok(m) = meta.modified() {
                                    files.push((p, m));
                                }
                            }
                        }
                    }
                }
            }
        }

        // Newest files first
        files.sort_by(|a, b| b.1.cmp(&a.1));

        // Read up to 5 most recent files; track latest rate_limits per limit_id
        let mut latest: HashMap<String, (CodexRateLimit, String)> = HashMap::new();
        for (path, _) in files.iter().take(5) {
            let Ok(content) = std::fs::read_to_string(path) else { continue };
            for line in content.lines() {
                let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else { continue };
                if val["type"].as_str() != Some("event_msg") { continue }
                let payload = &val["payload"];
                if payload["type"].as_str() != Some("token_count") { continue }
                let rl = &payload["rate_limits"];
                if rl.is_null() { continue }
                let Some(limit_id) = rl["limit_id"].as_str() else { continue };
                let (Some(prim), Some(sec)) = (rl.get("primary"), rl.get("secondary")) else { continue };

                let ts = val["timestamp"].as_str().unwrap_or("").to_string();
                let entry = CodexRateLimit {
                    limit_id: limit_id.to_string(),
                    limit_name: rl["limit_name"].as_str().map(|s| s.to_string()),
                    primary: CodexWindowInfo {
                        used_percent: prim["used_percent"].as_f64().unwrap_or(0.0),
                        resets_at: prim["resets_at"].as_u64().unwrap_or(0),
                        window_minutes: prim["window_minutes"].as_u64().unwrap_or(300),
                    },
                    secondary: CodexWindowInfo {
                        used_percent: sec["used_percent"].as_f64().unwrap_or(0.0),
                        resets_at: sec["resets_at"].as_u64().unwrap_or(0),
                        window_minutes: sec["window_minutes"].as_u64().unwrap_or(10080),
                    },
                };
                latest
                    .entry(limit_id.to_string())
                    .and_modify(|e| { if ts > e.1 { *e = (entry.clone(), ts.clone()); } })
                    .or_insert((entry, ts));
            }
        }

        let mut limits: Vec<CodexRateLimit> = latest.into_values().map(|(e, _)| e).collect();
        limits.sort_by(|a, b| a.limit_id.cmp(&b.limit_id));
        Ok(limits)
    })();

    match result {
        Ok(limits) if !limits.is_empty() => CodexRateLimitState { limits, read_at: now, error: None },
        Ok(_) => CodexRateLimitState { limits: vec![], read_at: now, error: Some("No Codex data".into()) },
        Err(e) => CodexRateLimitState { limits: vec![], read_at: now, error: Some(e) },
    }
}

// --- Claude Code Instance Detection ---

#[derive(Clone, Debug)]
struct ClaudeInstance {
    pid: u32,
    cwd: String,
    active: bool, // true = Claude is generating (user waiting)
}

/// Convert a CWD path to the ~/.claude/projects/ directory name.
/// e.g. "C:\Projects\ClaudeWatch" → "C--Projects-ClaudeWatch"
fn cwd_to_project_dir(cwd: &str) -> String {
    cwd.trim_end_matches(['\\', '/'])
        .replace(':', "-")
        .replace(['\\', '/'], "-")
}

/// Check if the most recently modified .jsonl file in the project dir
/// was updated within the last `threshold` seconds (= Claude is generating).
fn is_session_active(cwd: &str, threshold_secs: u64) -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let project_dir = home.join(".claude").join("projects").join(cwd_to_project_dir(cwd));
    let Ok(entries) = std::fs::read_dir(&project_dir) else {
        return false;
    };

    let now = std::time::SystemTime::now();
    let mut newest_mod = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                if newest_mod.map_or(true, |prev| modified > prev) {
                    newest_mod = Some(modified);
                }
            }
        }
    }

    newest_mod.map_or(false, |t| {
        now.duration_since(t)
            .map_or(false, |d| d.as_secs() < threshold_secs)
    })
}

fn detect_claude_instances() -> Vec<ClaudeInstance> {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_exe(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );

    let mut instances = Vec::new();
    for (pid, process) in sys.processes() {
        let exe_path = process
            .exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Native Windows installation: ~/.local/bin/claude.exe
        let is_claude_code = exe_path.contains(".local")
            && exe_path.ends_with("claude.exe");
        if !is_claude_code {
            continue;
        }

        let cwd = process
            .cwd()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        let active = is_session_active(&cwd, 15);

        instances.push(ClaudeInstance {
            pid: pid.as_u32(),
            cwd,
            active,
        });
    }
    instances
}

// --- App ---

struct ClaudeWatchApp {
    stats: Option<StatsCache>,
    last_stats_load: Instant,
    stats_error: Option<String>,
    rate_limit: Arc<Mutex<Option<RateLimitState>>>,
    codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
    instances: Arc<Mutex<Vec<ClaudeInstance>>>,
    last_content_height: f32,
    compact_mode: bool,
}

impl ClaudeWatchApp {
    fn new(
        rate_limit: Arc<Mutex<Option<RateLimitState>>>,
        codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
        instances: Arc<Mutex<Vec<ClaudeInstance>>>,
    ) -> Self {
        let mut app = Self {
            stats: None,
            last_stats_load: Instant::now(),
            stats_error: None,
            rate_limit,
            codex_rate_limit,
            instances,
            last_content_height: 0.0,
            compact_mode: true,
        };
        app.load_stats();
        app
    }

    fn stats_path() -> Option<std::path::PathBuf> {
        dirs::home_dir().map(|h| h.join(".claude").join("stats-cache.json"))
    }

    fn load_stats(&mut self) {
        let Some(path) = Self::stats_path() else {
            self.stats_error = Some("Home directory not found".into());
            return;
        };
        match std::fs::read_to_string(&path) {
            Ok(data) => match serde_json::from_str::<StatsCache>(&data) {
                Ok(stats) => {
                    self.stats = Some(stats);
                    self.stats_error = None;
                }
                Err(e) => self.stats_error = Some(format!("Parse: {e}")),
            },
            Err(e) => self.stats_error = Some(format!("Read: {e}")),
        }
        self.last_stats_load = Instant::now();
    }

    fn today_str() -> String {
        chrono::Local::now().format("%Y-%m-%d").to_string()
    }

    fn short_model_name(name: &str) -> &str {
        if name.contains("opus-4-6") {
            "Opus 4.6"
        } else if name.contains("opus-4-5") {
            "Opus 4.5"
        } else if name.contains("sonnet-4-5") {
            "Sonnet 4.5"
        } else if name.contains("haiku-4-5") {
            "Haiku 4.5"
        } else {
            name
        }
    }

    fn format_tokens(n: u64) -> String {
        if n >= 1_000_000_000 {
            format!("{:.1}B", n as f64 / 1_000_000_000.0)
        } else if n >= 1_000_000 {
            format!("{:.1}M", n as f64 / 1_000_000.0)
        } else if n >= 1_000 {
            format!("{:.1}K", n as f64 / 1_000.0)
        } else {
            n.to_string()
        }
    }

    fn format_reset_time(resets_at: &str) -> String {
        let Ok(reset) = chrono::DateTime::parse_from_rfc3339(resets_at) else {
            return "?".into();
        };
        let now = chrono::Utc::now();
        let diff = reset.signed_duration_since(now);
        let mins = diff.num_minutes();
        if mins <= 0 {
            "now".into()
        } else if mins < 60 {
            format!("{mins}min")
        } else {
            format!("{}h{}m", mins / 60, mins % 60)
        }
    }

    fn bar_color(pct: f64) -> egui::Color32 {
        if pct >= 80.0 {
            egui::Color32::from_rgb(30, 80, 220)
        } else if pct >= 50.0 {
            egui::Color32::from_rgb(50, 110, 200)
        } else {
            egui::Color32::from_rgb(60, 130, 190)
        }
    }

    fn codex_limit_label(limit: &CodexRateLimit) -> String {
        if let Some(ref name) = limit.limit_name {
            // Take the last '-'-separated segment (e.g. "GPT-5.3-Codex-Spark" → "Spark")
            name.rsplit('-').next().unwrap_or("Cx").to_string()
        } else {
            "Cx".to_string()
        }
    }

    fn format_codex_reset_time(resets_at: u64) -> String {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if resets_at <= now_secs {
            return "now".into();
        }
        let mins = (resets_at - now_secs) / 60;
        if mins < 60 {
            format!("{mins}min")
        } else {
            format!("{}h{}m", mins / 60, mins % 60)
        }
    }

    fn draw_usage_bar(ui: &mut egui::Ui, label: &str, pct: f64, reset_str: &str) {
        let bar_width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(bar_width, 18.0),
            egui::Sense::hover(),
        );

        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, egui::Color32::from_gray(50));

        let fill_width = bar_width * (pct as f32 / 100.0).min(1.0);
        if fill_width > 0.0 {
            let fill_rect =
                egui::Rect::from_min_size(rect.min, egui::vec2(fill_width, rect.height()));
            painter.rect_filled(fill_rect, 3.0, Self::bar_color(pct));
        }

        // Label on the left inside the bar
        painter.text(
            rect.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );

        // Percentage in the center
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{:.0}%", pct),
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );

        // Reset time on the right inside the bar
        painter.text(
            rect.right_center() - egui::vec2(6.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            reset_str,
            egui::FontId::proportional(10.0),
            egui::Color32::from_white_alpha(160),
        );
    }
}

impl eframe::App for ClaudeWatchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.last_stats_load.elapsed().as_secs() >= 30 {
            self.load_stats();
        }

        ctx.request_repaint_after(std::time::Duration::from_secs(5));

        // Reduce global spacing & disable text selection on labels (window is draggable)
        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(4.0, 2.0);
        style.interaction.selectable_labels = false;
        ctx.set_style(style);

        // Double-click to toggle compact/full view
        if ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            self.compact_mode = !self.compact_mode;
            self.last_content_height = 0.0;
        }

        // Drag anywhere to move
        if ctx.input(|i| i.pointer.primary_down()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        let close_hovered = std::cell::Cell::new(false);

        let panel_resp = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(6)))
            .show(ctx, |ui| {
                // Context menu (right-click)
                ui.interact(ui.max_rect(), ui.id().with("ctx_menu"), egui::Sense::click())
                    .context_menu(|ui| {
                        let label = if self.compact_mode { "Full View" } else { "Compact View" };
                        if ui.button(label).clicked() {
                            self.compact_mode = !self.compact_mode;
                            self.last_content_height = 0.0;
                            ui.close_menu();
                        }
                        if ui.button("Close").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            ui.close_menu();
                        }
                    });

                // Title bar + close button
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("ClaudeWatch").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = ui.small_button("X");
                        if btn.hovered() {
                            close_hovered.set(true);
                        }
                        if btn.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });

                // --- Running Instances ---
                {
                    let instances = self.instances.lock().unwrap().clone();
                    if instances.is_empty() {
                        ui.colored_label(
                            egui::Color32::from_rgb(120, 120, 120),
                            "No Claude Code running",
                        );
                    } else {
                        for inst in &instances {
                            let trimmed = inst.cwd.trim_end_matches(['\\', '/']);
                            let folder = trimmed
                                .rsplit_once(['\\', '/'])
                                .map(|(_, name)| name)
                                .unwrap_or(trimmed);

                            let (dot_color, status_text) = if inst.active {
                                (egui::Color32::from_rgb(80, 200, 80), "working")
                            } else {
                                (egui::Color32::from_rgb(120, 120, 120), "idle")
                            };

                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(8.0, 8.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(
                                    rect.center(),
                                    3.5,
                                    dot_color,
                                );
                                ui.label(
                                    egui::RichText::new(format!("{folder} ({status_text})"))
                                        .small()
                                        .color(if inst.active {
                                            egui::Color32::from_rgb(200, 200, 200)
                                        } else {
                                            egui::Color32::from_rgb(140, 140, 140)
                                        }),
                                )
                                .on_hover_text(format!("{} (PID:{})", inst.cwd, inst.pid));
                            });
                        }
                    }
                }

                ui.add_space(2.0);

                // --- Rate Limit ---
                let rl = self.rate_limit.lock().unwrap().clone();

                if let Some(ref state) = rl {
                    if let Some(ref err) = state.error {
                        // Show friendly message for common "not running" errors
                        let display = if err.starts_with("Read creds:")
                            || err == "No OAuth credentials"
                            || err == "Home directory not found"
                        {
                            "Waiting for Claude Code...".to_string()
                        } else if err.starts_with("Refresh failed")
                            || err.starts_with("Parse usage")
                            || err.starts_with("Usage API 401")
                        {
                            "Auth expired - restart Claude Code".to_string()
                        } else {
                            format!("Err: {err}")
                        };
                        ui.colored_label(
                            egui::Color32::from_rgb(150, 150, 150),
                            display,
                        );
                    } else {
                        let usage = &state.usage;
                        let mut has_limit = false;

                        if let Some(ref fh) = usage.five_hour {
                            Self::draw_usage_bar(
                                ui,
                                "5h",
                                fh.utilization,
                                &Self::format_reset_time(&fh.resets_at),
                            );
                            has_limit = true;
                        }

                        if let Some(ref sd) = usage.seven_day {
                            Self::draw_usage_bar(
                                ui,
                                "7d",
                                sd.utilization,
                                &Self::format_reset_time(&sd.resets_at),
                            );
                            has_limit = true;
                        }

                        if let Some(ref opus) = usage.seven_day_opus {
                            Self::draw_usage_bar(
                                ui,
                                "Opus",
                                opus.utilization,
                                &Self::format_reset_time(&opus.resets_at),
                            );
                            has_limit = true;
                        }

                        if let Some(ref sonnet) = usage.seven_day_sonnet {
                            Self::draw_usage_bar(
                                ui,
                                "Sonnet",
                                sonnet.utilization,
                                &Self::format_reset_time(&sonnet.resets_at),
                            );
                            has_limit = true;
                        }

                        if let Some(ref extra) = usage.extra_usage {
                            if extra.is_enabled {
                                let extra_label = format!(
                                    "${:.0}/${:.0}",
                                    extra.used_credits.unwrap_or(0.0), extra.monthly_limit.unwrap_or(0.0)
                                );
                                Self::draw_usage_bar(ui, &extra_label, extra.utilization.unwrap_or(0.0), "extra");
                                has_limit = true;
                            }
                        }

                        if !has_limit {
                            ui.label("No active limits");
                        }

                        ui.label(
                            egui::RichText::new(state.fetched_at.format("%H:%M:%S").to_string())
                                .weak(),
                        );
                    }
                } else {
                    ui.label("Fetching...");
                }

                // --- Codex Rate Limit ---
                let codex_rl = self.codex_rate_limit.lock().unwrap().clone();
                if let Some(ref codex_state) = codex_rl {
                    if let Some(ref err) = codex_state.error {
                        if !err.contains("not installed") && !err.contains("No Codex data") {
                            ui.colored_label(
                                egui::Color32::from_rgb(150, 150, 150),
                                format!("Codex: {err}"),
                            );
                        }
                    } else {
                        ui.add_space(2.0);
                        for limit in &codex_state.limits {
                            let base = Self::codex_limit_label(limit);
                            Self::draw_usage_bar(
                                ui,
                                &format!("{base} 5h"),
                                limit.primary.used_percent,
                                &Self::format_codex_reset_time(limit.primary.resets_at),
                            );
                            Self::draw_usage_bar(
                                ui,
                                &format!("{base} 7d"),
                                limit.secondary.used_percent,
                                &Self::format_codex_reset_time(limit.secondary.resets_at),
                            );
                        }
                    }
                }

                if !self.compact_mode {
                    ui.separator();

                    // --- Stats ---
                    if let Some(ref err) = self.stats_error {
                        ui.colored_label(egui::Color32::from_rgb(200, 100, 50), err);
                        return ui.cursor().top();
                    }

                    let Some(stats) = &self.stats else {
                        return ui.cursor().top();
                    };

                    let today = Self::today_str();
                    let today_activity = stats.daily_activity.iter().find(|a| a.date == today);

                    egui::Grid::new("today")
                        .num_columns(4)
                        .spacing([8.0, 2.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Today").strong());
                            if let Some(act) = today_activity {
                                ui.label(format!("{}msg", act.message_count));
                                ui.label(format!("{}ses", act.session_count));
                                ui.label(format!("{}tool", act.tool_call_count));
                            } else {
                                ui.label("-");
                            }
                            ui.end_row();

                            ui.label(egui::RichText::new("Total").strong());
                            ui.label(format!("{}msg", stats.total_messages));
                            ui.label(format!("{}ses", stats.total_sessions));
                            ui.label("");
                            ui.end_row();
                        });

                    ui.separator();

                    // Model Usage
                    let mut models: Vec<_> = stats.model_usage.iter().collect();
                    models.sort_by_key(|(name, _)| name.to_string());

                    egui::Grid::new("models")
                        .num_columns(3)
                        .spacing([8.0, 2.0])
                        .show(ui, |ui| {
                            for (name, usage) in &models {
                                ui.label(Self::short_model_name(name));
                                ui.label(format!(
                                    "i:{} o:{}",
                                    Self::format_tokens(usage.input_tokens),
                                    Self::format_tokens(usage.output_tokens)
                                ));
                                ui.label(format!(
                                    "c:{}",
                                    Self::format_tokens(usage.cache_read_input_tokens)
                                ));
                                ui.end_row();
                            }
                        });
                }

                // Return content bottom position
                ui.cursor().top()
            });

        // Auto-resize window to fit content (no bottom gap)
        let desired_height = panel_resp.inner + 6.0; // + bottom margin
        if (self.last_content_height - desired_height).abs() > 2.0 {
            self.last_content_height = desired_height;
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                300.0,
                desired_height,
            )));
        }

        // Show grab cursor on draggable areas, but not on buttons or popup menus
        let over_popup = ctx.input(|i| i.pointer.hover_pos()).map_or(false, |pos| {
            ctx.layer_id_at(pos).map_or(false, |id| {
                id.order == egui::Order::Foreground || id.order == egui::Order::Tooltip
            })
        });

        if !close_hovered.get() && !over_popup {
            let is_dragging = ctx.input(|i| i.pointer.primary_down());
            ctx.output_mut(|o| {
                if o.cursor_icon == egui::CursorIcon::Default
                    || o.cursor_icon == egui::CursorIcon::Text
                {
                    o.cursor_icon = if is_dragging {
                        egui::CursorIcon::Grabbing
                    } else {
                        egui::CursorIcon::Grab
                    };
                }
            });
        }
    }
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
    std::thread::spawn(move || loop {
        let state = fetch_rate_limit();
        *rl_clone.lock().unwrap() = Some(state);
        std::thread::sleep(std::time::Duration::from_secs(60));
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
        Box::new(move |_cc| Ok(Box::new(ClaudeWatchApp::new(rl_for_app, codex_rl_for_app, inst_for_app)))),
    )
}
