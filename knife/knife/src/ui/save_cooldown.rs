//! The save cooldown's 60 Hz clock, shared by the story and save/load screens.

use super::Screen;

pub(super) fn tick(remaining: u32, screen: Screen, stack: &[Screen], title_state: u32) -> u32 {
    let story = screen == Screen::Game || stack.contains(&Screen::Game);
    // WoH's load list and its dialogs run inside Title state 25 rather than
    // Screen::SaveLoad. Keep the inherited TeaTime/menu write cooldown ticking
    // there too, or ws_list's confirm/delete gate can stay closed forever.
    let load = matches!(screen, Screen::SaveLoad { .. })
        || stack.iter().any(|s| matches!(s, Screen::SaveLoad { .. }))
        || (screen == Screen::Title && title_state == 25);
    if story || load { remaining.saturating_sub(1) } else { remaining }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teatime_title_then_load_list_expires_the_save_cooldown() {
        // TeaReq::TitleSave starts 180 frames. The banner and story fade use
        // some of them; time spent on the title must not strand the remainder
        // when its embedded load list opens (jp_8E200 state 25).
        let mut remaining = 180;
        for _ in 0..125 {
            remaining = tick(remaining, Screen::Game, &[], 0);
        }
        assert_eq!(remaining, 55);
        for _ in 0..300 {
            remaining = tick(remaining, Screen::Title, &[], 11);
        }
        assert_eq!(remaining, 55);
        for _ in 0..54 {
            remaining = tick(remaining, Screen::Title, &[], 25);
        }
        assert_eq!(remaining, 1);
        assert_eq!(tick(remaining, Screen::Title, &[], 25), 0);
        assert_eq!(tick(0, Screen::Title, &[], 25), 0);
    }

    #[test]
    fn save_load_clocks_keep_running_under_dialogs_and_menus() {
        for (screen, stack) in [
            (Screen::Game, vec![]),
            (Screen::Witch, vec![Screen::Game]),
            (Screen::Menu, vec![Screen::Game]),
            (Screen::SaveLoad { save: false, scroll: 0 }, vec![]),
            (Screen::SaveLoad { save: true, scroll: 0 }, vec![]),
            (Screen::Config, vec![Screen::SaveLoad { save: false, scroll: 0 }]),
        ] {
            assert_eq!(tick(1, screen, &stack, 0), 0, "{screen:?}");
            assert_eq!(tick(0, screen, &stack, 0), 0, "{screen:?}");
        }
    }

    #[test]
    fn title_config_and_extra_screens_do_not_consume_the_cooldown() {
        for state in [0, 2, 9, 11, 26, 35, 38, 39, 40] {
            assert_eq!(tick(55, Screen::Title, &[], state), 55);
        }
        // A retained title load state must not make other screens count down.
        for screen in [Screen::Config, Screen::Witch] {
            assert_eq!(tick(55, screen, &[], 25), 55);
        }
    }
}
