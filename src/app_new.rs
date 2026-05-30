use crate::rate_limit::RateLimitState;
use crate::codex::CodexRateLimitState;
use crate::instances::ClaudeInstance;
use crate::visibility::VisibilityControl;
use eframe::egui;
use std::sync::{Arc, Mutex};

pub struct ClaudeWatchApp {
    rate_limit: Arc<Mutex<Option<RateLimitState>>>,
    codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
    instances: Arc<Mutex<Vec<ClaudeInstance>>>,
    visibility_control: Arc<VisibilityControl>,
}

impl ClaudeWatchApp {
    pub fn new(
        rate_limit: Arc<Mutex<Option<RateLimitState>>>,
        codex_rate_limit: Arc<Mutex<Option<CodexRateLimitState>>>,
        instances: Arc<Mutex<Vec<ClaudeInstance>>>,
        visibility_control: Arc<VisibilityControl>,
    ) -> Self {
        Self {
            rate_limit,
            codex_rate_limit,
            instances,
            visibility_control,
        }
    }
}

impl eframe::App for ClaudeWatchApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // update native HWND once
        if self
            .visibility_control
            .hwnd
            .load(std::sync::atomic::Ordering::Relaxed)
            == 0
        {
            if let Some(hwnd) = crate::platform::frame_hwnd(_frame) {
                self.visibility_control
                    .hwnd
                    .store(hwnd, std::sync::atomic::Ordering::Relaxed);
            }
        }

        // update dragging state from pointer + native button
        let dragging = ctx.input(|i| i.pointer.primary_down()) && crate::platform::is_left_mouse_down();
        self.visibility_control
            .dragging
            .store(dragging, std::sync::atomic::Ordering::Relaxed);

        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label("ClaudeWatch (running)");
        });
    }
}
