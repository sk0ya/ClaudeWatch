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
    stay_for_idle: bool,
}

impl VisibilityState {
    fn new(now: Instant) -> Self {
        Self {
            display_until: now + STARTUP_VISIBLE_DURATION,
            last_input: last_input_tick(),
            was_dragging: false,
            visible: true,
            stay_for_idle: false,
        }
    }

    fn update_inner(
        &mut self,
        now: Instant,
        idle_dur: Option<Duration>,
        input_tick: Option<u32>,
        is_dragging: bool,
    ) -> Option<bool> {
        if is_dragging && !self.was_dragging {
            self.stay_for_idle = false;
            self.display_until = now + HIDE_AFTER_DRAG;
        }
        self.was_dragging = is_dragging;

        if is_dragging {
            self.display_until = now + HIDE_AFTER_DRAG;
        }

        let input_changed = self.last_input.is_some() && input_tick != self.last_input;
        // Only start the hide countdown if currently shown due to idle, not during startup
        if input_changed && self.stay_for_idle {
            self.stay_for_idle = false;
            self.display_until = now + HIDE_AFTER_INPUT;
        }
        self.last_input = input_tick;

        if let Some(d) = idle_dur {
            if d >= SHOW_AFTER_IDLE && !self.visible {
                self.stay_for_idle = true;
            }
        }

        let should_be_visible = now < self.display_until || self.stay_for_idle;
        if self.visible != should_be_visible {
            self.visible = should_be_visible;
            Some(self.visible)
        } else {
            None
        }
    }

    fn update(&mut self, now: Instant, hwnd: isize, control: &VisibilityControl) {
        let changed = self.update_inner(
            now,
            user_idle_duration(),
            last_input_tick(),
            control.dragging.load(Ordering::Relaxed),
        );
        if let Some(visible) = changed {
            show_window(hwnd, visible);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(now: Instant) -> VisibilityState {
        VisibilityState {
            display_until: now + STARTUP_VISIBLE_DURATION,
            last_input: Some(1000),
            was_dragging: false,
            visible: true,
            stay_for_idle: false,
        }
    }

    fn tick(state: &mut VisibilityState, now: Instant, idle_secs: u64, input_tick: u32) {
        state.update_inner(
            now,
            Some(Duration::from_secs(idle_secs)),
            Some(input_tick),
            false,
        );
    }

    // ─── update_inner が Some(false) を返すことで show_window が呼ばれる ──
    // hwnd==0 のままだと show_window は呼ばれないため HWND が必須
    #[test]
    fn update_inner_returns_some_on_visibility_change() {
        let t0 = Instant::now();
        let mut s = make_state(t0);
        // 5.1秒後: visible が true→false に変化するので Some(false) が返るはず
        let result = s.update_inner(
            t0 + Duration::from_millis(5100),
            Some(Duration::from_secs(10)),
            Some(1000),
            false,
        );
        assert_eq!(result, Some(false), "show_window(hwnd, false) が呼ばれるべき変化");
    }

    // ─── 起動後5秒で非表示になる ───────────────────────────────
    #[test]
    fn startup_hides_after_5s() {
        let t0 = Instant::now();
        let mut s = make_state(t0);

        tick(&mut s, t0 + Duration::from_secs(4), 10, 1000);
        assert!(s.visible, "4s: まだ表示中のはず");

        tick(&mut s, t0 + Duration::from_millis(5100), 10, 1000);
        assert!(!s.visible, "5.1s: 非表示になるはず");
    }

    // ─── 起動中にマウス/キー操作しても5秒で非表示 ──────────────
    #[test]
    fn startup_hides_even_with_input() {
        let t0 = Instant::now();
        let mut s = make_state(t0);

        // 毎100msで input_tick が変わる（マウス移動 or キー入力を模擬）
        let mut tick_val: u32 = 1000;
        let mut now = t0;
        for _ in 0..60 {
            now += Duration::from_millis(100);
            tick_val += 1;
            tick(&mut s, now, 0, tick_val); // idle=0（アクティブ）
        }
        // 6秒後: 起動5秒ウィンドウは終わっている
        assert!(!s.visible, "入力があっても6s後は非表示のはず (actual: {})", s.visible);
    }

    // ─── アイドル60秒で表示 ─────────────────────────────────────
    #[test]
    fn shows_after_60s_idle() {
        let t0 = Instant::now();
        let mut s = make_state(t0);

        // 起動5秒が過ぎて非表示にする
        tick(&mut s, t0 + Duration::from_millis(5100), 10, 1000);
        assert!(!s.visible, "前提: 非表示");

        // アイドル60秒→表示
        tick(&mut s, t0 + Duration::from_secs(70), 60, 1000);
        assert!(s.visible, "アイドル60sで表示になるはず");
        assert!(s.stay_for_idle, "stay_for_idle が立つはず");
    }

    // ─── アイドル表示中に入力→5秒後に非表示 ───────────────────
    #[test]
    fn hides_5s_after_input_during_idle_show() {
        let t0 = Instant::now();
        let mut s = make_state(t0);

        tick(&mut s, t0 + Duration::from_millis(5100), 10, 1000);
        assert!(!s.visible);

        // アイドル60sで表示
        tick(&mut s, t0 + Duration::from_secs(70), 65, 1000);
        assert!(s.visible);

        // キー入力（input_tick が変わる）
        let input_time = t0 + Duration::from_secs(71);
        tick(&mut s, input_time, 1, 1001);
        assert!(s.visible, "入力直後はまだ表示中");
        assert!(!s.stay_for_idle, "stay_for_idle は解除される");

        // 4.9s後: まだ表示
        tick(&mut s, input_time + Duration::from_millis(4900), 0, 1001);
        assert!(s.visible, "4.9s後: まだ表示");

        // 5.1s後: 非表示
        tick(&mut s, input_time + Duration::from_millis(5100), 0, 1001);
        assert!(!s.visible, "5.1s後: 非表示のはず");
    }

    // ─── 非表示中に入力しても表示しない ────────────────────────
    #[test]
    fn no_show_on_input_while_hidden() {
        let t0 = Instant::now();
        let mut s = make_state(t0);

        tick(&mut s, t0 + Duration::from_millis(5100), 10, 1000);
        assert!(!s.visible);

        // 入力しても表示しない（アイドルではないので）
        tick(&mut s, t0 + Duration::from_secs(6), 0, 1001);
        assert!(!s.visible, "非表示中の入力で表示してはいけない");
    }
}
