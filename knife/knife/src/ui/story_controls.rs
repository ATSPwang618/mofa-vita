//! Player-facing story navigation policy. Engine seek/load is deliberately separate.

#[derive(Clone, Copy)]
pub(super) struct StoryControls {
    pub fast_forward: bool,
    pub rewind: bool,
    pub scene_jump: bool,
}

pub(super) const STORY_CONTROLS: StoryControls = StoryControls::for_vita(cfg!(target_os = "vita"));

impl StoryControls {
    const fn for_vita(vita: bool) -> Self {
        Self { fast_forward: !vita, rewind: !vita, scene_jump: !vita }
    }

    /// Zero-based panel indices: previous, rewind, auto, fast-forward, next, save, load, voice.
    pub const fn panel_button(self, button: usize) -> bool {
        match button {
            0 | 4 => self.scene_jump,
            1 => self.rewind,
            3 => self.fast_forward,
            2 | 5 | 6 | 7 => true,
            _ => false,
        }
    }

    /// Skip disabled controls; like the original panel, only a fresh press wraps.
    pub fn panel_focus(self, current: usize, right: bool, repeat: bool) -> Option<usize> {
        let mut next = current as i32;
        for _ in 0..8 {
            next += if right { 1 } else { -1 };
            if !(0..8).contains(&next) {
                if repeat {
                    return None;
                }
                next = next.rem_euclid(8);
            }
            if self.panel_button(next as usize) {
                return (next as usize != current).then_some(next as usize);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{STORY_CONTROLS, StoryControls};

    #[test]
    fn vita_disables_only_the_four_marked_buttons() {
        let policy = StoryControls::for_vita(true);
        let actual: Vec<_> = (0..8).filter(|&i| policy.panel_button(i)).collect();
        assert_eq!(actual, [2, 5, 6, 7]);
        assert!(!policy.fast_forward && !policy.rewind && !policy.scene_jump);
        assert!(!policy.panel_button(8));
    }

    #[test]
    fn vita_focus_skips_disabled_controls_and_wraps_on_fresh_press() {
        let policy = StoryControls::for_vita(true);
        for (from, right) in [(2, 5), (5, 6), (6, 7), (7, 2)] {
            assert_eq!(policy.panel_focus(from, true, false), Some(right));
            assert_eq!(policy.panel_focus(right, false, false), Some(from));
        }
    }

    #[test]
    fn held_navigation_stops_at_the_first_and_last_enabled_buttons() {
        let policy = StoryControls::for_vita(true);
        assert_eq!(policy.panel_focus(2, false, true), None);
        assert_eq!(policy.panel_focus(7, true, true), None);
        assert_eq!(policy.panel_focus(2, true, true), Some(5));
        assert_eq!(policy.panel_focus(5, false, true), Some(2));
    }

    #[test]
    fn stale_disabled_focus_can_only_move_to_an_enabled_control() {
        let policy = StoryControls::for_vita(true);
        for current in [0, 1, 3, 4] {
            assert!(!policy.panel_button(current));
            for right in [false, true] {
                let next = policy.panel_focus(current, right, false).unwrap();
                assert!(policy.panel_button(next));
            }
        }
    }

    #[test]
    fn desktop_keeps_all_controls_and_original_navigation() {
        let policy = StoryControls::for_vita(false);
        assert!(policy.fast_forward && policy.rewind && policy.scene_jump);
        for i in 0..8 {
            assert!(policy.panel_button(i));
            assert_eq!(policy.panel_focus(i, true, false), Some((i + 1) % 8));
            assert_eq!(policy.panel_focus(i, false, false), Some((i + 7) % 8));
        }
    }

    #[test]
    fn production_policy_matches_the_target() {
        for i in 0..8 {
            assert_eq!(STORY_CONTROLS.panel_button(i), StoryControls::for_vita(cfg!(target_os = "vita")).panel_button(i));
        }
    }
}
