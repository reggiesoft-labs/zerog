use std::time::{Duration, Instant};

const CLICK_DISTANCE: f64 = 4.0;

#[derive(Clone, Copy)]
struct Click {
    time: Instant,
    position: (f64, f64),
    count: i64,
}

#[derive(Default)]
pub(super) struct ClickState {
    buttons: [Option<Click>; 8],
}

impl ClickState {
    pub(super) fn press(
        &mut self,
        button: u8,
        position: (f64, f64),
        now: Instant,
        interval: Duration,
    ) -> i64 {
        let previous = self.buttons[usize::from(button)];
        let mut count = 1;
        if let Some(click) = previous {
            let nearby = (position.0 - click.position.0).abs() <= CLICK_DISTANCE
                && (position.1 - click.position.1).abs() <= CLICK_DISTANCE;
            if now.duration_since(click.time) <= interval && nearby {
                count = click.count + 1;
            }
        }
        self.buttons[usize::from(button)] = Some(Click {
            time: now,
            position,
            count,
        });
        count
    }

    pub(super) fn count(&self, button: u8) -> i64 {
        self.buttons[usize::from(button)].map_or(1, |click| click.count)
    }

    pub(super) fn reset(&mut self) {
        self.buttons = [None; 8];
    }
}

#[cfg(test)]
mod tests {
    use super::ClickState;
    use std::time::{Duration, Instant};

    const INTERVAL: Duration = Duration::from_millis(500);

    #[test]
    fn down_and_up_counts_match_for_single_double_and_triple_clicks() {
        let mut state = ClickState::default();
        let start = Instant::now();
        for button in 0..8 {
            for count in 1..=3 {
                let now = start + Duration::from_millis(count * 100);
                assert_eq!(
                    state.press(button, (20.0, 30.0), now, INTERVAL),
                    count as i64
                );
                assert_eq!(state.count(button), count as i64);
            }
        }
    }

    #[test]
    fn timeout_and_distance_restart_the_sequence() {
        let start = Instant::now();
        for (delay, position, expected) in [
            (500, (24.0, 26.0), 2),
            (501, (20.0, 30.0), 1),
            (100, (24.1, 30.0), 1),
            (100, (20.0, 34.1), 1),
        ] {
            let mut state = ClickState::default();
            assert_eq!(state.press(0, (20.0, 30.0), start, INTERVAL), 1);
            assert_eq!(
                state.press(0, position, start + Duration::from_millis(delay), INTERVAL),
                expected
            );
            assert_eq!(state.count(0), expected);
        }
    }

    #[test]
    fn buttons_keep_independent_counts() {
        let mut state = ClickState::default();
        let start = Instant::now();
        assert_eq!(state.press(0, (20.0, 30.0), start, INTERVAL), 1);
        assert_eq!(state.press(1, (20.0, 30.0), start, INTERVAL), 1);
        assert_eq!(state.press(0, (20.0, 30.0), start, INTERVAL), 2);
        assert_eq!(state.count(1), 1);
        assert_eq!(state.count(0), 2);
    }

    #[test]
    fn cleanup_restarts_clicks_even_before_the_old_interval_expires() {
        let mut state = ClickState::default();
        let start = Instant::now();
        for button in 0..8 {
            assert_eq!(state.press(button, (20.0, 30.0), start, INTERVAL), 1);
            assert_eq!(state.press(button, (20.0, 30.0), start, INTERVAL), 2);
        }
        state.reset();
        for button in 0..8 {
            assert_eq!(
                state.press(button, (20.0, 30.0), start + INTERVAL / 2, INTERVAL),
                1
            );
            assert_eq!(state.count(button), 1);
        }
    }
}
