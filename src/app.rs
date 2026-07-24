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

    fn remaining_minutes(resets_at: &str) -> Option<i64> {
        chrono::DateTime::parse_from_rfc3339(resets_at)
            .ok()
            .map(|reset| reset.signed_duration_since(chrono::Utc::now()).num_minutes())
    }

    /// Codex reports reset times as Unix seconds rather than RFC3339.
    fn remaining_minutes_unix(resets_at: u64) -> Option<i64> {
        if resets_at == 0 {
            return None;
        }
        chrono::DateTime::from_timestamp(resets_at as i64, 0)
            .map(|reset| reset.signed_duration_since(chrono::Utc::now()).num_minutes())
    }

    fn format_reset(remaining: Option<i64>) -> String {
        match remaining {
            Some(mins) => Self::format_remaining(mins),
            None => "?".into(),
        }
    }

    /// How far the window itself has advanced, as a percentage. Compared against
    /// token utilization this shows whether usage is ahead of or behind pace.
    fn elapsed_pct(remaining: Option<i64>, window_mins: i64) -> Option<f64> {
        let remaining = remaining?;
        if window_mins <= 0 {
            return None;
        }
        let elapsed = (window_mins - remaining.max(0)) as f64;
        Some((elapsed / window_mins as f64 * 100.0).clamp(0.0, 100.0))
    }

    fn draw_usage_bar(
        ui: &mut egui::Ui,
        label: &str,
        pct: f64,
        reset_str: &str,
        time_pct: Option<f64>,
    ) {
        let bar_width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(bar_width, 18.0), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, egui::Color32::from_gray(50));
        let fill_width = bar_width * (pct as f32 / 100.0).min(1.0);
        if fill_width > 0.0 {
            let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_width, rect.height()));
            painter.rect_filled(fill_rect, 3.0, egui::Color32::from_rgb(40, 110, 200));
        }
        // Pace marker: the share of the window that has already elapsed. Fill left of
        // the marker means tokens are being spent slower than the clock, right means faster.
        if let Some(t) = time_pct {
            let x = rect.left() + bar_width * (t as f32 / 100.0).clamp(0.0, 1.0);
            painter.line_segment(
                [egui::pos2(x, rect.top() + 1.0), egui::pos2(x, rect.bottom() - 1.0)],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(250, 210, 80)),
            );
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
                            let rem = Self::remaining_minutes(&fh.resets_at);
                            Self::draw_usage_bar(
                                ui,
                                "5h",
                                fh.utilization,
                                &Self::format_reset(rem),
                                Self::elapsed_pct(rem, 5 * 60),
                            );
                        }
                        if let Some(sd) = usage.seven_day {
                            let rem = Self::remaining_minutes(&sd.resets_at);
                            Self::draw_usage_bar(
                                ui,
                                "7d",
                                sd.utilization,
                                &Self::format_reset(rem),
                                Self::elapsed_pct(rem, 7 * 24 * 60),
                            );
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
                                let rem = Self::remaining_minutes_unix(w.resets_at);
                                Self::draw_usage_bar(
                                    ui,
                                    &format!("{display_name} {}", w.window_label()),
                                    w.used_percent,
                                    &Self::format_reset(rem),
                                    Self::elapsed_pct(rem, w.window_minutes as i64),
                                );
                            }
                        }
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::ClaudeWatchApp as App;

    #[test]
    fn elapsed_pct_tracks_window_progress() {
        // 5h window with 3h left => 40% of the window has elapsed.
        assert_eq!(App::elapsed_pct(Some(180), 300), Some(40.0));
        assert_eq!(App::elapsed_pct(Some(300), 300), Some(0.0));
        assert_eq!(App::elapsed_pct(Some(0), 300), Some(100.0));
    }

    #[test]
    fn elapsed_pct_clamps_odd_inputs() {
        // Reset already passed, or further out than the window width.
        assert_eq!(App::elapsed_pct(Some(-30), 300), Some(100.0));
        assert_eq!(App::elapsed_pct(Some(400), 300), Some(0.0));
        // No reset time, or an unknown window width.
        assert_eq!(App::elapsed_pct(None, 300), None);
        assert_eq!(App::elapsed_pct(Some(180), 0), None);
    }
}
