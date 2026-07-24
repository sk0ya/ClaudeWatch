use crate::rate_limit::RateLimitState;
use crate::codex::CodexRateLimitState;
use crate::instances::ClaudeInstance;
use crate::visibility::VisibilityControl;
use crate::platform::{frame_hwnd, is_left_mouse_down};
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ClaudeWatchApp {
    stats: Option<crate::StatsCache>,
    last_stats_load: Instant,
    stats_error: Option<String>,
    rate_limit: Arc<Mutex<Option<RateLimitState>>>,
    codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
    instances: Arc<Mutex<Vec<ClaudeInstance>>>,
    visibility_control: Arc<VisibilityControl>,
    last_content_height: f32,
    compact_mode: bool,
}

impl ClaudeWatchApp {
    pub fn new(
        rate_limit: Arc<Mutex<Option<RateLimitState>>>,
        codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
        instances: Arc<Mutex<Vec<ClaudeInstance>>>,
        visibility_control: Arc<VisibilityControl>,
    ) -> Self {
        let mut app = Self {
            stats: None,
            last_stats_load: Instant::now(),
            stats_error: None,
            rate_limit,
            codex_rate_limit,
            instances,
            visibility_control,
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
        if let Some(path) = Self::stats_path() {
            match std::fs::read_to_string(&path) {
                Ok(data) => match serde_json::from_str::<crate::StatsCache>(&data) {
                    Ok(stats) => {
                        self.stats = Some(stats);
                        self.stats_error = None;
                    }
                    Err(e) => self.stats_error = Some(format!("Parse: {e}")),
                },
                Err(e) => self.stats_error = Some(format!("Read: {e}")),
            }
        } else {
            self.stats_error = Some("Home directory not found".into());
        }
        self.last_stats_load = Instant::now();
    }

    fn format_remaining(mins: i64) -> String {
        if mins <= 0 {
            "now".into()
        } else if mins < 60 {
            format!("{mins}min")
        } else if mins < 1440 {
            format!("{}h{}m", mins / 60, mins % 60)
        } else {
            format!("{}d{}h{}m", mins / 1440, (mins % 1440) / 60, mins % 60)
        }
    }

    fn format_reset_time(resets_at: &str) -> String {
        if let Ok(reset) = chrono::DateTime::parse_from_rfc3339(resets_at) {
            let diff = reset.signed_duration_since(chrono::Utc::now());
            Self::format_remaining(diff.num_minutes())
        } else {
            "?".into()
        }
    }

    /// Codex reports reset times as Unix seconds rather than RFC3339.
    fn format_reset_unix(resets_at: u64) -> String {
        if resets_at == 0 {
            return "?".into();
        }
        match chrono::DateTime::from_timestamp(resets_at as i64, 0) {
            Some(reset) => {
                let diff = reset.signed_duration_since(chrono::Utc::now());
                Self::format_remaining(diff.num_minutes())
            }
            None => "?".into(),
        }
    }

    fn draw_usage_bar(ui: &mut egui::Ui, label: &str, pct: f64, reset_str: &str) {
        let bar_width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_width, 18.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, egui::Color32::from_gray(50));
        let fill_width = bar_width * (pct as f32 / 100.0).min(1.0);
        if fill_width > 0.0 {
            let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_width, rect.height()));
            painter.rect_filled(fill_rect, 3.0, egui::Color32::from_rgb(40, 110, 200));
        }
        painter.text(
            rect.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
        painter.text(
            rect.right_center() - egui::vec2(6.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            reset_str,
            egui::FontId::proportional(10.0),
            egui::Color32::from_white_alpha(180),
        );
    }
}

impl eframe::App for ClaudeWatchApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // ensure native hwnd is set for visibility control
        if self.visibility_control.hwnd.load(std::sync::atomic::Ordering::Relaxed) == 0 {
            if let Some(hwnd) = frame_hwnd(frame) {
                self.visibility_control.hwnd.store(hwnd, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // update dragging from UI + native button state
        let dragging = ctx.input(|i| i.pointer.primary_down()) && is_left_mouse_down();
        self.visibility_control
            .dragging
            .store(dragging, std::sync::atomic::Ordering::Relaxed);

        if self.last_stats_load.elapsed().as_secs() >= 30 {
            self.load_stats();
        }

        ctx.request_repaint_after(Duration::from_millis(250));

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(4.0, 2.0);
        ctx.set_style(style);

        if ctx.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
            self.compact_mode = !self.compact_mode;
            self.last_content_height = 0.0;
        }

        if ctx.input(|i| i.pointer.primary_down()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(6)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("ClaudeWatch").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("X").clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });

                ui.add_space(6.0);

                if let Some(stats) = &self.stats {
                    ui.horizontal(|ui| {
                        ui.label(format!("Sessions: {}", stats.total_sessions));
                        ui.add_space(12.0);
                        ui.label(format!("Messages: {}", stats.total_messages));
                    });
                    if let Some(latest) = stats.daily_activity.last() {
                        ui.label(format!("Latest activity: {} ({})", latest.date, latest.messages));
                    }
                    if let Some((model, usage)) = stats.model_usage.iter().next() {
                        ui.label(format!("{}: {} tokens, {} messages", model, usage.tokens, usage.messages));
                    }
                    ui.add_space(6.0);
                }

                // Instances
                let instances = self.instances.lock().unwrap().clone();
                if instances.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(120, 120, 120), "No Claude Code running");
                } else {
                    for inst in &instances {
                        let trimmed = inst.cwd.trim_end_matches(['\\', '/']);
                        let folder = trimmed.rsplit_once(['\\', '/']).map(|(_, name)| name).unwrap_or(trimmed);
                        let (dot_color, status_text) = if inst.active {
                            (egui::Color32::from_rgb(80, 200, 80), "working")
                        } else {
                            (egui::Color32::from_rgb(140, 140, 140), "idle")
                        };
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                            ui.painter().circle_filled(rect.center(), 3.5, dot_color);
                            ui.label(egui::RichText::new(format!("{folder} ({status_text}) PID:{pid}", pid = inst.pid)).small());
                        });
                    }
                }

                ui.add_space(6.0);

                // Rate limits
                let rl = self.rate_limit.lock().unwrap().clone();
                if let Some(state) = rl {
                    if let Some(err) = state.error {
                        ui.colored_label(egui::Color32::from_rgb(150, 150, 150), format!("Err: {err}"));
                    } else {
                        let usage = state.usage;
                        if let Some(fh) = usage.five_hour {
                            Self::draw_usage_bar(ui, "5h", fh.utilization, &Self::format_reset_time(&fh.resets_at));
                        }
                        if let Some(sd) = usage.seven_day {
                            Self::draw_usage_bar(ui, "7d", sd.utilization, &Self::format_reset_time(&sd.resets_at));
                        }
                    }
                }

                ui.add_space(6.0);

                // Codex
                let codex = self.codex_rate_limit.lock().unwrap().clone();
                if let Some(cstate) = codex {
                    if let Some(_err) = cstate.error {
                        ui.colored_label(egui::Color32::from_rgb(120, 120, 120), "No Codex data");
                    } else if !cstate.limits.is_empty() {
                        for l in &cstate.limits {
                            let display_name = l.limit_name.as_deref().unwrap_or(&l.limit_id);
                            for w in std::iter::once(&l.primary).chain(l.secondary.iter()) {
                                Self::draw_usage_bar(
                                    ui,
                                    &format!("{display_name} {}", w.window_label()),
                                    w.used_percent,
                                    &Self::format_reset_unix(w.resets_at),
                                );
                            }
                        }
                    }
                }
            });
    }
}
