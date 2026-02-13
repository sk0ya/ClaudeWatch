#![windows_subsystem = "windows"]

use eframe::egui;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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

#[derive(Deserialize, Clone, Debug)]
struct ExtraUsageEntry {
    is_enabled: bool,
    monthly_limit: f64,
    used_credits: f64,
    utilization: f64,
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

// --- App ---

struct ClaudeWatchApp {
    stats: Option<StatsCache>,
    last_stats_load: Instant,
    stats_error: Option<String>,
    rate_limit: Arc<Mutex<Option<RateLimitState>>>,
    last_content_height: f32,
}

impl ClaudeWatchApp {
    fn new(rate_limit: Arc<Mutex<Option<RateLimitState>>>) -> Self {
        let mut app = Self {
            stats: None,
            last_stats_load: Instant::now(),
            stats_error: None,
            rate_limit,
            last_content_height: 0.0,
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

        // Drag anywhere to move
        if ctx.input(|i| i.pointer.primary_down()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        let panel_resp = egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(6)))
            .show(ctx, |ui| {
                // Close button row
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("ClaudeWatch").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("X").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });

                // --- Rate Limit ---
                let rl = self.rate_limit.lock().unwrap().clone();

                if let Some(ref state) = rl {
                    if let Some(ref err) = state.error {
                        ui.colored_label(egui::Color32::from_rgb(200, 100, 50), format!("Err: {err}"));
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
                                    extra.used_credits, extra.monthly_limit
                                );
                                Self::draw_usage_bar(ui, &extra_label, extra.utilization, "extra");
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

        // Show grab cursor on draggable areas (but not on buttons)
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

    // Background thread for rate limit polling
    let rl_clone = Arc::clone(&rate_limit);
    std::thread::spawn(move || loop {
        let state = fetch_rate_limit();
        *rl_clone.lock().unwrap() = Some(state);
        std::thread::sleep(std::time::Duration::from_secs(60));
    });

    let rl_for_app = Arc::clone(&rate_limit);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([300.0, 260.0])
            .with_always_on_top()
            .with_title("ClaudeWatch")
            .with_decorations(false)
            .with_icon(load_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "ClaudeWatch",
        options,
        Box::new(move |_cc| Ok(Box::new(ClaudeWatchApp::new(rl_for_app)))),
    )
}
