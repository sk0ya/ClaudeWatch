use crate::rate_limit::RateLimitState;
use crate::codex::CodexRateLimitState;
use crate::visibility::VisibilityControl;
use crate::platform::{frame_hwnd, is_left_mouse_down};
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BAR_HEIGHT: f32 = 18.0;
const BAR_SPACING: f32 = 2.0;
const MARGIN: f32 = 6.0;

/// One usage bar, ready to draw.
struct Bar {
    label: String,
    pct: f64,
    reset: String,
    time_pct: Option<f64>,
}

pub struct ClaudeWatchApp {
    rate_limit: Arc<Mutex<Option<RateLimitState>>>,
    codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
    visibility_control: Arc<VisibilityControl>,
    last_window_height: f32,
}

impl ClaudeWatchApp {
    pub fn new(
        rate_limit: Arc<Mutex<Option<RateLimitState>>>,
        codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
        visibility_control: Arc<VisibilityControl>,
    ) -> Self {
        Self {
            rate_limit,
            codex_rate_limit,
            visibility_control,
            last_window_height: 0.0,
        }
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

    /// Collect every limit worth a bar, Claude first then Codex.
    fn collect_bars(&self) -> Vec<Bar> {
        let mut bars = Vec::new();

        if let Some(state) = self.rate_limit.lock().unwrap().clone() {
            if state.error.is_none() {
                if let Some(fh) = state.usage.five_hour {
                    let rem = Self::remaining_minutes(&fh.resets_at);
                    bars.push(Bar {
                        label: "5h".into(),
                        pct: fh.utilization,
                        reset: Self::format_reset(rem),
                        time_pct: Self::elapsed_pct(rem, 5 * 60),
                    });
                }
                if let Some(sd) = state.usage.seven_day {
                    let rem = Self::remaining_minutes(&sd.resets_at);
                    bars.push(Bar {
                        label: "7d".into(),
                        pct: sd.utilization,
                        reset: Self::format_reset(rem),
                        time_pct: Self::elapsed_pct(rem, 7 * 24 * 60),
                    });
                }
            }
        }

        if let Some(cstate) = self.codex_rate_limit.lock().unwrap().clone() {
            if cstate.error.is_none() {
                for l in &cstate.limits {
                    let display_name = l.limit_name.as_deref().unwrap_or(&l.limit_id);
                    for w in std::iter::once(&l.primary).chain(l.secondary.iter()) {
                        let rem = Self::remaining_minutes_unix(w.resets_at);
                        bars.push(Bar {
                            label: format!("{display_name} {}", w.window_label()),
                            pct: w.used_percent,
                            reset: Self::format_reset(rem),
                            time_pct: Self::elapsed_pct(rem, w.window_minutes as i64),
                        });
                    }
                }
            }
        }

        bars
    }

    fn draw_usage_bar(ui: &mut egui::Ui, bar: &Bar) {
        let bar_width = ui.available_width();
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(bar_width, BAR_HEIGHT), egui::Sense::hover());
        let painter = ui.painter();
        painter.rect_filled(rect, 3.0, egui::Color32::from_gray(50));
        let fill_width = bar_width * (bar.pct as f32 / 100.0).min(1.0);
        if fill_width > 0.0 {
            let fill_rect = egui::Rect::from_min_size(rect.min, egui::vec2(fill_width, rect.height()));
            painter.rect_filled(fill_rect, 3.0, egui::Color32::from_rgb(40, 110, 200));
        }
        // Pace marker: the share of the window that has already elapsed. Fill left of
        // the marker means tokens are being spent slower than the clock, right means faster.
        if let Some(t) = bar.time_pct {
            let x = rect.left() + bar_width * (t as f32 / 100.0).clamp(0.0, 1.0);
            painter.line_segment(
                [egui::pos2(x, rect.top() + 1.0), egui::pos2(x, rect.bottom() - 1.0)],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(250, 210, 80)),
            );
        }
        painter.text(
            rect.left_center() + egui::vec2(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            &bar.label,
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
        painter.text(
            rect.right_center() - egui::vec2(6.0, 0.0),
            egui::Align2::RIGHT_CENTER,
            &bar.reset,
            egui::FontId::proportional(10.0),
            egui::Color32::from_white_alpha(180),
        );
    }

    /// Height the window needs to show exactly this many bars.
    fn window_height(bar_count: usize) -> f32 {
        let rows = bar_count.max(1) as f32;
        MARGIN * 2.0 + rows * BAR_HEIGHT + (rows - 1.0) * BAR_SPACING
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

        ctx.request_repaint_after(Duration::from_millis(250));

        let mut style = (*ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(4.0, BAR_SPACING);
        ctx.set_style(style);

        // The close button is the only chrome left, and it only appears on hover.
        let mut close_hovered = false;
        if ctx.input(|i| i.pointer.hover_pos()).is_some() {
            let resp = egui::Area::new("close".into())
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-2.0, 2.0))
                .show(ctx, |ui| ui.small_button("x"))
                .inner;
            close_hovered = resp.hovered();
            if resp.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // Dragging anywhere moves the window, except over the close button.
        if ctx.input(|i| i.pointer.primary_down()) && !close_hovered {
            ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }

        let bars = self.collect_bars();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.inner_margin(egui::Margin::same(MARGIN as i8)))
            .show(ctx, |ui| {
                if bars.is_empty() {
                    ui.colored_label(egui::Color32::from_gray(120), "no limit data");
                }
                for bar in &bars {
                    Self::draw_usage_bar(ui, bar);
                }
            });

        // Keep the window exactly as tall as the bars it shows.
        let desired = Self::window_height(bars.len());
        if (desired - self.last_window_height).abs() > 0.5 {
            self.last_window_height = desired;
            let width = ctx.input(|i| i.screen_rect().width());
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, desired)));
        }
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

    #[test]
    fn window_height_grows_one_row_at_a_time() {
        // margin*2 + rows*18 + gaps*2
        assert_eq!(App::window_height(1), 30.0);
        assert_eq!(App::window_height(2), 50.0);
        assert_eq!(App::window_height(4), 90.0);
        // The empty state still needs one row of space.
        assert_eq!(App::window_height(0), App::window_height(1));
    }
}
