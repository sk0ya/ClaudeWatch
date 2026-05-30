use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::platform::{last_input_tick, user_idle_duration, show_window};

pub const SHOW_AFTER_IDLE: Duration = Duration::from_secs(60);
pub const HIDE_AFTER_INPUT: Duration = Duration::from_secs(5);
pub const HIDE_AFTER_DRAG: Duration = Duration::from_secs(3);
pub const STARTUP_VISIBLE_DURATION: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct VisibilityControl {
    pub hwnd: AtomicIsize,
    pub dragging: AtomicBool,
}

struct VisibilityState {
    display_until: Instant,
    last_input: Option<u32>,
    was_dragging: bool,
    visible: bool,
}

impl VisibilityState {
    fn new(now: Instant) -> Self {
        Self {
            display_until: now + STARTUP_VISIBLE_DURATION,
            last_input: last_input_tick(),
            was_dragging: false,
            visible: true,
        }
    }

    fn update(&mut self, now: Instant, hwnd: isize, control: &VisibilityControl) {
        let is_dragging = control.dragging.load(Ordering::Relaxed);
        let current_input = last_input_tick();

        if is_dragging && !self.was_dragging {
            self.display_until = now + HIDE_AFTER_DRAG;
        }
        self.was_dragging = is_dragging;

        if is_dragging {
            self.display_until = now + HIDE_AFTER_DRAG;
        }

        let input_changed = self.last_input.is_some() && current_input != self.last_input;
        if input_changed {
            self.display_until = now + HIDE_AFTER_INPUT;
        }
        self.last_input = current_input;

        if let Some(idle_dur) = user_idle_duration() {
            if idle_dur >= SHOW_AFTER_IDLE {
                self.display_until = now + HIDE_AFTER_INPUT;
            }
        }

        let should_be_visible = now < self.display_until;
        if self.visible != should_be_visible {
            self.visible = should_be_visible;
            show_window(hwnd, self.visible);
        }
    }
}

pub fn start_visibility_thread(control: Arc<VisibilityControl>, _initial_visible: bool) {
    std::thread::spawn(move || {
        let started_at = Instant::now();
        let mut state = VisibilityState::new(started_at);

        loop {
            let hwnd = control.hwnd.load(Ordering::Relaxed);
            if hwnd == 0 {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }

            let now = Instant::now();
            state.update(now, hwnd, &control);
            std::thread::sleep(Duration::from_millis(100));
        }
    });
}

