//! The save / load screen: jp_4EF00 installs it (page tabs 28..33 from the table at 0x3CF258, the twenty
//! slot objects 8..27 without frames, the cursor from the system block), sub_323A0 gives the tabs the save or
//! load look, sub_324D0 runs it (state off_A360B4, alpha dword_A36090 at 0.024 a frame, the slots' animation
//! in the state's high half: 0..2), sub_302A0 draws it and sub_31F30 builds the line under the cursor.
//!
//! Six pages of twenty records (0..119); the storage checks of the Switch (jp_B66A0, jp_B6690, jp_B6700, the
//! record status words) always pass here, so their error states are left out; a failed file write shows
//! "セーブに失敗しました" and asks whether to stop saving, as states 9 / 10 do.

use knife_render::{Blend, Quad, RenderTarget, Renderer, Shade};

use super::super::{H, Ui, W};
use super::{DOWN, LEFT, MASK, RIGHT, UP, objtab};
use crate::game::Game;
use crate::savedata;

const SX: f32 = 0.33333;
const SY: f32 = 0.41481;
/// the first slot object (jp_ADCA0(8 + n, 2, 0, unk_3CE9A8: rect (-3, -2)-(86, 62)))
const SLOT_OBJ: i32 = 8;
/// the first page tab (dword_3CF258)
const TAB_OBJ: i32 = 28;
/// the page tabs' x (sub_302A0)
const TAB_X: [f32; 6] = [140.0, 206.67, 273.33, 340.0, 406.67, 473.33];
/// aFff (0x261950): each slot's delay of the page slide, sliding out (st 3 / 27: towards the left, then to the
/// right) and in (st 4 / 28: from the right, then from the left)
const SLIDE: [[f32; 20]; 4] = [
    [0.9, 0.75, 0.6, 0.45, 0.3, 0.8, 0.65, 0.5, 0.35, 0.2, 0.7, 0.55, 0.4, 0.25, 0.1, 0.6, 0.45, 0.3, 0.15, 0.0],
    [0.3, 0.45, 0.6, 0.75, 0.9, 0.2, 0.35, 0.5, 0.65, 0.8, 0.1, 0.25, 0.4, 0.55, 0.7, 0.0, 0.15, 0.3, 0.45, 0.6],
    [0.6, 0.45, 0.3, 0.15, 0.0, 0.7, 0.55, 0.4, 0.25, 0.1, 0.8, 0.65, 0.5, 0.35, 0.2, 0.9, 0.75, 0.6, 0.45, 0.3],
    [0.0, 0.15, 0.3, 0.45, 0.6, 0.1, 0.25, 0.4, 0.55, 0.7, 0.2, 0.35, 0.5, 0.65, 0.8, 0.3, 0.45, 0.6, 0.75, 0.9],
];
/// dword_261B30: the archive icon (SAVE_PARTS 13, 56 px cells) per scene 1..245
const SCENE_ICON: [u8; 245] = [
    8, 9, 9, 9, 9, 4, 5, 5, 5, 5, 0, 0, 0, 0, 0, 1, 5, 4, 6, 6, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 7, 5, 5, 0, 1, 1, 4, 4, 7, 3, 0, 1, 1, 1, 1, 1, 1, 7, 6, 3, 3, 5, 1, 1, 4, 2, 4, 4,
    4, 7, 7, 1, 1, 1, 0, 0, 4, 7, 7, 7, 7, 4, 4, 3, 3, 1, 1, 1, 1, 1, 1, 9, 9, 9, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2,
    2, 0, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 8, 4, 4, 4, 4, 4, 4, 0, 7, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
    7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
];

#[derive(Default)]
pub struct WSave {
    installed: bool,
    /// LODWORD(off_A360B4)
    pub st: u32,
    /// HIDWORD(off_A360B4): the slots' animation, 2 at rest
    anim: f32,
    /// dword_A36090
    pub a: f32,
    /// dword_A36094: the save screen (else load)
    pub save: bool,
    /// qword_A36098: the page shown and the tab cursor
    page: i32,
    tab: i32,
    /// dword_A360A0: the page whose thumbnails are up
    shown: i32,
    /// qword_A360A4: the slot cursor and the slot the line under the slots was built for
    slot: i32,
    slot_seen: i32,
    /// qword_A360AC
    col: i32,
    row: i32,
    /// LODWORD(qword_A360BC): the page slide's direction
    dir: f32,
    /// HIDWORD(qword_A360BC) / LODWORD(qword_A360C4): the drag of a held finger, springing back by 60 a frame
    drag: [i32; 2],
    /// dword_A360D0: the popup's minimum frames
    wait: i32,
    /// dword_A360D4: B is ignored for these frames after a write
    lock: i32,
    /// dword_A360D8: sub_324D0's result once closed (2 loaded, 3 back)
    result: i32,
    /// dword_A360DC: the page before the last page change
    prev_page: i32,
    /// dword_A360E0: the record acted on
    target: usize,
    /// byte_A36074: the cursor is on the page tabs
    tabrow: bool,
    /// word_A36088 / word_A3608C: the copy's source
    src: (i32, i32),
    /// byte_A36070[132 + n]: record n's thumbnail was drawn
    drawn: Vec<bool>,
    /// the page and slot sub_31F30 last built the line for
    line: (i32, i32),
    /// the backdrop's offscreen surfaces
    #[cfg(not(target_os = "vita"))]
    rt: Option<[knife_render::TexRef; 2]>,
    /// opened from the title (jp_8E200 state 25): a load leaves the record for the title to start
    pub title: bool,
    /// the record a load read
    pub loaded: Option<usize>,
}

impl WSave {
    fn focus_slot(&mut self, slot: i32, focus: &mut i32) {
        self.slot = slot;
        self.col = slot % 5;
        self.row = slot / 5;
        self.tabrow = false;
        *focus = SLOT_OBJ + slot;
    }

    fn focus_tab(&mut self, tab: i32, focus: &mut i32) {
        self.tab = tab;
        self.tabrow = true;
        *focus = TAB_OBJ + tab;
    }

    fn reopen_focus(&mut self, focus: &mut i32) {
        self.tab = self.page;
        self.focus_slot(self.slot, focus);
    }

    // Only an actual pointer event may override pad navigation. WObjs.hit can
    // retain a synthetic hit from a page slide (lock=1) after the pointer left.
    fn resolve_focus(&mut self, hit: i32, pointer: bool, focus: &mut i32) -> bool {
        let old = (self.tabrow, self.tab, self.slot);
        if pointer && (TAB_OBJ..TAB_OBJ + 6).contains(&hit) {
            self.focus_tab(hit - TAB_OBJ, focus);
        } else if pointer && (SLOT_OBJ..SLOT_OBJ + 20).contains(&hit) {
            self.focus_slot(hit - SLOT_OBJ, focus);
        } else if self.tabrow {
            self.focus_tab(self.tab, focus);
        } else {
            self.focus_slot(self.slot, focus);
        }
        old != (self.tabrow, self.tab, self.slot)
    }

    fn move_grid(&mut self, pressed: u32, repeat: u32, focus: &mut i32) -> bool {
        let old = (self.tabrow, self.slot);
        let row = self.row;
        if repeat & UP != 0 {
            if row > 0 { self.focus_slot(self.slot - 5, focus); }
            else if pressed & UP != 0 { self.focus_tab(self.page, focus); }
        } else if repeat & DOWN != 0 {
            if row < 3 { self.focus_slot(self.slot + 5, focus); }
            else if pressed & DOWN != 0 { self.focus_tab(self.page, focus); }
        } else if repeat & RIGHT != 0 {
            if self.slot < 19 { self.focus_slot(self.slot + 1, focus); }
            else if pressed & RIGHT != 0 { self.focus_slot(0, focus); }
        } else if repeat & LEFT != 0 {
            if self.slot > 0 { self.focus_slot(self.slot - 1, focus); }
            else if pressed & LEFT != 0 { self.focus_slot(19, focus); }
        }
        debug_assert_eq!((self.col, self.row), (self.slot % 5, self.slot / 5));
        old != (self.tabrow, self.slot)
    }
}

impl Ui {
    /// Small change key: no strings/allocations on unchanged frames.
    #[cfg(target_os = "vita")]
    pub(crate) fn vita_save_focus(&self) -> Option<[i32; 10]> {
        let s = &self.w.save;
        (s.a > 0.0).then(|| [s.st as i32, s.save as i32, s.page, s.slot,
            s.col, s.row, s.tabrow as i32, self.w.objs.focus, self.w.objs.hit,
            self.w.tex.get(415).is_some() as i32])
    }

    /// Read-only, typed save UI observation for finite Vita DEV validation.
    #[cfg(target_os = "vita")]
    pub(crate) fn vita_diagnostic_save(&self, game: &Game) -> String {
        let s = &self.w.save;
        let selected_used = (0..6).contains(&s.page) && (0..20).contains(&s.slot)
            && self.file.record(s.page as usize * 20 + s.slot as usize).is_some();
        let point = game.vita_diagnostic_save_point().map_or_else(
            || "null".to_owned(),
            |(scene, pc)| format!("{{\"scene\":{scene},\"pc\":{pc}}}"),
        );
        format!(concat!(
            "{{\"state\":{},\"saving\":{},\"page\":{},\"slot\":{},\"tabrow\":{},",
            "\"lock\":{},\"target\":{},\"manual_count\":{},\"selected_used\":{},",
            "\"dialog_active\":{},\"dialog_answer\":{},\"popup_busy\":{},\"cool\":{},",
            "\"can_save\":{},\"point\":{}}}"
        ), s.st, s.save, s.page, s.slot, s.tabrow, s.lock, s.target,
            self.file.manual_count(), selected_used, self.w.dlg.active, self.w.dlg.answer,
            self.w.popup.busy, self.save_cool, game.can_save(), point)
    }

    /// jp_4EF00 with sub_2F280(0): the cursor on the newest list record (the system block's +5528, written by
    /// sub_2F560 mode 2).
    fn ws_install(&mut self, game: &Game, r: &Renderer) {
        if self.w.save.installed {
            return;
        }
        self.w.tex.load(game, r, objtab::RES_SAVE);
        // the guide's help_key / help_txt come with the game screen's parts (jp_3B9A0)
        if self.w.tex.get(76).is_none() {
            self.w.tex.load(game, r, objtab::RES_DISP);
        }
        self.w.objs.install(objtab::SAVE);
        for k in 0..20 {
            self.w.objs.create(SLOT_OBJ + k, 2, 0, [-3, -2, 86, 62]);
        }
        let (page, slot) = self.file.continue_cursor();
        let s = &mut self.w.save;
        s.installed = true;
        s.drawn = vec![false; 120];
        s.page = i32::from(page);
        s.tab = s.page;
        s.shown = s.page;
        s.slot = i32::from(slot);
        s.slot_seen = s.slot;
        s.col = s.slot % 5;
        s.row = s.slot / 5;
        s.dir = 1.0;
    }

    /// The menu's Save / Load (sub_2D940: jp_4EA10(save), sub_323A0(save)), then sub_31A70 as the menu
    /// fades.
    pub(in crate::ui) fn ws_open(&mut self, game: &mut Game, r: &Renderer, save: bool) {
        self.ws_install(game, r);
        self.wd_install(game, r);
        if self.sysmes.is_empty() {
            self.sysmes = game.assets.sysmes();
        }
        self.w.save.save = save;
        self.w.save.title = false;
        self.w.save.loaded = None;
        self.w.save.reopen_focus(&mut self.w.objs.focus);
        self.w.objs.reset_hit();
        for k in 0..9 {
            self.w.objs.scale(TAB_OBJ + k, SX, SY);
        }
        let looks = if save { &objtab::SAVE_TABS_SAVE } else { &objtab::SAVE_TABS_LOAD };
        for (k, st) in looks.iter().enumerate() {
            self.w.objs.set_states(TAB_OBJ + k as i32, st);
        }
        self.w.save.shown = self.w.save.page;
        self.reload_slots(game);
    }

    fn ws_used(&self, n: usize) -> bool {
        self.file.record(n).is_some()
    }

    /// sub_31F30(page): the line under the slots follows `page` and the cursor.
    fn ws_line(&mut self, page: i32) {
        self.w.save.line = (page, self.w.save.slot);
    }

    /// jp_ADE80 on the old and the new page's tab (flag 2 marks the page shown).
    fn ws_tab_flags(&mut self, old: i32, new: i32) {
        self.w.objs.flags(TAB_OBJ + old, 0, 2);
        self.w.objs.flags(TAB_OBJ + new, 2, 0);
    }

    /// sub_324D0, one 60 Hz frame: 2 once a record was loaded and the screen is gone, 3 once closed, else 0.
    pub(in crate::ui) fn ws_step(&mut self, game: &mut Game, r: &Renderer) -> i32 {
        let s = &mut self.w.save;
        if s.wait >= 1 {
            s.wait -= 1;
        }
        // a held finger drags the slots along its stronger axis (4 units of play); let go, they spring back
        match self.w.inp.held_delta() {
            Some([dx, dy]) => {
                let (dx, dy) = (dx as i32, dy as i32);
                let play = |v: i32| if v <= -5 { v + 4 } else { (v - 4).max(0) };
                s.drag = if dx.abs() > dy.abs() { [play(dx), 0] } else { [0, play(dy)] };
            }
            None => {
                for d in &mut s.drag {
                    *d = if *d >= 1 { (*d - 60).max(0) } else { (*d + 60).min(0) };
                }
            }
        }
        match s.st {
            0 | 1 => {
                if s.st == 0 {
                    s.st = 1;
                    let p = s.page;
                    self.ws_line(p);
                    self.w.guide.on = true;
                }
                let s = &mut self.w.save;
                if s.a < 1.0 {
                    s.a = (s.a + 0.024).min(1.0);
                }
                s.anim = s.a * 2.0;
                if s.anim >= 2.0 {
                    s.st = 2;
                    s.anim = 2.0;
                    let page = s.page;
                    self.w.objs.group_flags(2, 1, 0);
                    self.w.objs.flags(TAB_OBJ + page, 2, 0);
                    self.ws_line(page);
                    self.reload_slots(game);
                }
            }
            2 => return self.ws_list(game, r, false),
            26 => return self.ws_list(game, r, true),
            3 | 27 => {
                self.w.objs.lock = 1;
                s.anim -= 0.085;
                if s.anim > 0.0 {
                    return 0;
                }
                s.st = if s.st == 3 { 4 } else { 28 };
                s.anim = 0.0;
                s.shown = s.page;
                let p = s.page;
                self.ws_line(p);
            }
            4 | 28 => {
                s.anim += 0.085;
                if s.anim < 2.0 {
                    return 0;
                }
                s.anim = 2.0;
                self.w.objs.lock = 0;
                s.st = if s.st == 4 { 2 } else { 26 };
            }
            5 => {
                if s.a > 0.0 {
                    s.a = (s.a - 0.024).max(0.0);
                }
                s.anim = s.a * 2.0;
                if s.anim > 0.0 {
                    return 0;
                }
                s.anim = 0.0;
                s.st = 0;
                self.w.inp.pos = [320.0, 224.0];
                self.touch.at = [320.0, 224.0];
                self.w.objs.group_flags(2, 0, 3);
                return self.w.save.result;
            }
            6 => {
                // save: the dialog's answer
                if self.w.dlg.busy() {
                    return 0;
                }
                s.st = if self.w.dlg.answer == 0 { 7 } else { 2 };
            }
            7 => {
                if self.w.popup.busy != 0 {
                    return 0;
                }
                // "セーブ中です" held for 30 frames at least, the record written (sub_353B0, sub_2F560(2),
                // jp_B6520) and the cooldown set (jp_40E70(180))
                self.wp_open(game, 159, 1200);
                self.wp_hold();
                self.w.save.wait = 30;
                let n = self.w.save.target;
                let ok = game.save_data().is_some_and(|d| match self.write_record(game, n, &d) {
                    Ok(()) => true,
                    Err(e) => {
                        tracing::warn!("save to slot {n}: {e:#}");
                        false
                    }
                });
                self.save_cool = 180;
                self.w.save.st = if ok { 8 } else { 9 };
                if !ok {
                    self.wp_close();
                    self.wp_open(game, 165, 480);
                }
            }
            8 | 25 | 31 => {
                if s.wait > 0 {
                    return 0;
                }
                // the write is done: sound 13, the slot's thumbnail made again, B ignored for 40 frames
                self.wp_close();
                game.play_sys_sound(13);
                let n = self.w.save.target;
                game.clear_previews(Some(n));
                self.reload_slots(game);
                let p = self.w.save.page;
                self.ws_line(p);
                self.w.save.lock = 40;
                self.w.save.st = 2;
            }
            9 => {
                // the write failed: once the popup is gone, dialog 7 (セーブをキャンセルしますか？)
                if self.w.popup.busy != 0 {
                    return 0;
                }
                s.st = 10;
                self.wd_open(game, r, 7, true, false);
            }
            10 => {
                if self.w.dlg.busy() {
                    return 0;
                }
                if self.w.dlg.answer == 0 {
                    self.w.save.result = 3;
                    self.w.save.st = 5;
                    self.w.guide.on = false;
                } else {
                    self.w.save.st = 2;
                }
            }
            17 => {
                // load: the dialog's answer; "ロード中です" held
                if self.w.dlg.busy() {
                    return 0;
                }
                if self.w.dlg.answer == 0 {
                    self.wp_open(game, 160, 480);
                    self.wp_hold();
                    self.w.save.st = 18;
                } else {
                    self.w.save.st = 2;
                }
            }
            18 => {
                // once the popup is up: sound 13, the record read into the game (sub_652B0), the screen fades
                if self.w.popup.busy == 1 {
                    return 0;
                }
                game.play_sys_sound(13);
                let n = self.w.save.target;
                self.wp_close();
                if self.w.save.title {
                    // the title starts the story from it after its fade (state 23)
                    self.w.save.loaded = Some(n);
                    self.w.save.result = 2;
                } else if self.restore_slot(game, r, n) {
                    self.w.save.result = 2;
                } else {
                    self.w.save.result = 3;
                }
                self.w.save.st = 5;
                self.w.guide.on = false;
            }
            19 | 22 | 32 => {
                if self.w.popup.busy != 0 {
                    return 0;
                }
                s.st = 2;
            }
            23 => {
                // delete: the dialog's answer
                if self.w.dlg.busy() {
                    return 0;
                }
                s.st = if self.w.dlg.answer == 0 { 24 } else { 2 };
            }
            24 => {
                if self.w.popup.busy != 0 {
                    return 0;
                }
                self.wp_open(game, 164, 1200);
                self.wp_hold();
                let n = self.w.save.target;
                self.file.delete_record(n);
                self.file.update_continue();
                self.ws_store(game, 24);
                self.save_cool = 180;
                let s = &mut self.w.save;
                s.wait = 30;
                s.drawn[n] = false;
                game.clear_previews(Some(n));
            }
            29 => {
                // copy: the dialog's answer
                if self.w.dlg.busy() {
                    return 0;
                }
                s.st = if self.w.dlg.answer == 0 { 30 } else { 26 };
            }
            30 => {
                if self.w.popup.busy != 0 {
                    return 0;
                }
                self.wp_open(game, 163, 1200);
                self.wp_hold();
                let (n, (sp, ss)) = (self.w.save.target, self.w.save.src);
                if let Some(rec) = self.file.record((sp * 20 + ss) as usize).map(<[u8]>::to_vec) {
                    self.file.set_record(n, &rec);
                    self.file.update_continue();
                }
                self.ws_store(game, 30);
                self.save_cool = 180;
                let s = &mut self.w.save;
                s.wait = 30;
                s.drawn[n] = true;
                game.clear_previews(Some(n));
            }
            _ => {}
        }
        0
    }

    /// The file write of a delete (state 24) or a copy (30); on to 25 / 31, or the failure popup.
    fn ws_store(&mut self, game: &mut Game, st: u32) {
        game.store_system(&mut self.file);
        match self.file.store() {
            Ok(()) => self.w.save.st = st + 1,
            Err(e) => {
                tracing::warn!("writing the save file: {e:#}");
                self.wp_close();
                self.wp_open(game, 165, 480);
                self.w.save.st = 9;
            }
        }
        self.reload_slots(game);
    }

    /// States 2 (the list) and 26 (copy: picking the copy's place).
    fn ws_list(&mut self, game: &mut Game, r: &Renderer, copy: bool) -> i32 {
        let p = self.w.inp.pressed;
        let rep = self.w.inp.repeat;
        let touch = self.w.inp.touch;
        let flick = self.w.inp.flick;
        let s = &mut self.w.save;
        if !copy {
            if s.lock >= 1 {
                s.lock -= 1;
            }
            if (p & MASK[1] != 0 && s.lock < 1) || self.w.guide.tapped {
                self.w.zl_block = true;
                self.w.save.result = 3;
                self.w.save.st = 5;
                self.w.guide.on = false;
                game.play_sys_sound(1);
                return 0;
            }
        } else if p & MASK[1] != 0 || touch & 8 != 0 {
            s.st = 2;
            game.play_sys_sound(1);
            return 0;
        }
        let page_st = if copy { 27 } else { 3 };
        // A release tap is valid even though WIn.active is already false.
        let pointer = self.w.inp.active || touch & 0x2002 != 0;
        let hit = if pointer { self.w.objs.hit } else { -2 };
        if self.w.save.resolve_focus(hit, pointer, &mut self.w.objs.focus) {
            game.play_sys_sound(2);
        }
        let t = hit.wrapping_sub(TAB_OBJ);
        if self.w.save.tabrow {
            let s = &mut self.w.save;
            let v192 = s.tab;
            if s.page != v192 && (p & MASK[0] != 0 || (v192 == t && touch & 0x2000 != 0)) {
                s.prev_page = s.page;
                self.ws_page(game, page_st);
                return 0;
            }
            if p & UP != 0 {
                s.focus_slot(s.col + 15, &mut self.w.objs.focus);
                game.play_sys_sound(2);
                return 0;
            }
            if p & DOWN != 0 {
                s.focus_slot(s.col, &mut self.w.objs.focus);
                game.play_sys_sound(2);
                return 0;
            }
            let to = if rep & RIGHT != 0 {
                if v192 <= 4 { v192 + 1 } else if p & RIGHT != 0 { 0 } else { v192 }
            } else if rep & LEFT != 0 {
                if v192 >= 1 { v192 - 1 } else if p & LEFT != 0 { 5 } else { v192 }
            } else {
                let next = if p & MASK[9] != 0 || flick & 1 != 0 {
                    if s.page <= 4 { s.page + 1 } else { 0 }
                } else if p & MASK[8] != 0 || flick & 2 != 0 {
                    if s.page < 1 { 5 } else { s.page - 1 }
                } else {
                    return 0;
                };
                s.prev_page = s.page;
                s.tab = next;
                self.w.objs.focus = TAB_OBJ + next;
                self.ws_page(game, page_st);
                return 0;
            };
            s.tab = to;
            if v192 != to {
                self.w.objs.focus = TAB_OBJ + to;
                game.play_sys_sound(2);
            }
            return 0;
        }
        let s = &mut self.w.save;
        let n = (s.slot + 20 * s.page) as usize;
        if p & MASK[0] != 0 || (touch & 2 != 0 && hit == s.slot + SLOT_OBJ) {
            if copy {
                if (s.page, s.slot) == s.src || s.page > 5 {
                    game.play_sys_sound(6);
                } else {
                    s.target = n;
                    s.st = 29;
                    if self.ws_used(n) {
                        self.wd_open(game, r, 2, true, false);
                    } else {
                        self.wd_open(game, r, 22, false, false);
                    }
                }
                self.ws_seen();
                return 0;
            }
            // (sub_40E90: nothing while the save cooldown runs)
            if self.save_cool < 1 {
                let used = self.ws_used(n);
                let s = &mut self.w.save;
                s.target = n;
                if s.save {
                    if !game.can_save() {
                        return 0;
                    }
                    s.st = 6;
                    if used {
                        self.wd_open(game, r, 2, true, false);
                    } else {
                        self.wd_open(game, r, 1, false, false);
                    }
                } else if used {
                    s.st = 17;
                    self.wd_open(game, r, 0, false, false);
                } else {
                    s.st = 19;
                    self.wp_open(game, 169, 480);
                    game.play_sys_sound(1);
                }
                self.ws_seen();
                return 0;
            }
        }
        let s = &mut self.w.save;
        if rep & (UP | DOWN | RIGHT | LEFT) != 0 {
            if s.move_grid(p, rep, &mut self.w.objs.focus) {
                game.play_sys_sound(2);
            }
        } else if p & MASK[9] != 0 || flick & 1 != 0 {
            s.prev_page = s.page;
            let next = if s.page <= 4 { s.page + 1 } else { 0 };
            self.ws_tab_flags(self.w.save.page, next);
            let s = &mut self.w.save;
            s.page = next;
            s.st = page_st;
            s.dir = -1.0;
            game.play_sys_sound(0);
        } else if p & MASK[8] != 0 || flick & 2 != 0 {
            s.prev_page = s.page;
            let next = if s.page < 1 { 5 } else { s.page - 1 };
            self.ws_tab_flags(self.w.save.page, next);
            let s = &mut self.w.save;
            s.page = next;
            s.st = page_st;
            s.dir = 1.0;
            game.play_sys_sound(0);
        } else if !copy && p & 1 != 0 {
            // Minus: the page of the newest record
            s.prev_page = s.page;
            let newest = i32::from(self.file.continue_cursor().0);
            if s.page != newest {
                self.ws_tab_flags(self.w.save.page, newest);
                let s = &mut self.w.save;
                s.page = newest;
                s.dir = 1.0;
                s.st = 3;
                game.play_sys_sound(0);
            }
        } else if !copy && p & 0x8000 != 0 {
            // Y: copy from a used record
            s.target = n;
            if self.ws_used(n) {
                let s = &mut self.w.save;
                s.src = (s.page, s.slot);
                s.st = 26;
                game.play_sys_sound(0);
            } else {
                self.w.save.st = 32;
                self.wp_open(game, 169, 480);
                game.play_sys_sound(1);
            }
        } else if !copy && p & 0x1000 != 0 && self.save_cool < 1 {
            // X: delete a used record
            s.target = n;
            if self.ws_used(n) {
                self.w.save.st = 23;
                self.wd_open(game, r, 21, true, false);
            } else {
                self.w.save.st = 22;
                self.wp_open(game, 169, 480);
                game.play_sys_sound(1);
            }
        }
        // (LABEL_456 also keeps the cursor in word_A3607C / word_A36080, which only sub_2F560 mode 1 writes
        // to the file, and nothing asks for that mode)
        self.ws_seen();
        0
    }

    /// LABEL_381 / 382: the page changes to the tab cursor's (the slide's direction by the order).
    fn ws_page(&mut self, game: &mut Game, st: u32) {
        let (old, new) = (self.w.save.page, self.w.save.tab);
        self.ws_tab_flags(old, new);
        self.w.objs.press(TAB_OBJ + new, 10);
        let s = &mut self.w.save;
        s.st = st;
        s.dir = if s.page >= s.tab { 1.0 } else { -1.0 };
        game.play_sys_sound(0);
        s.page = s.tab;
    }

    /// LABEL_584: a new slot under the cursor rebuilds the line (sub_31F30).
    fn ws_seen(&mut self) {
        let s = &mut self.w.save;
        if s.slot_seen != s.slot {
            s.slot_seen = s.slot;
            let p = s.page;
            self.ws_line(p);
        }
    }

    /// sub_302A0.
    pub(in crate::ui) fn ws_draw(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>) {
        let s = &self.w.save;
        let a = s.a;
        let mut pen = super::Pen::default();
        pen.scale(SX, SY);
        if a > 0.0 {
            self.ws_backdrop(game, r, out, a);
            pen.alpha(a);
            let s = &self.w.save;
            if let Some(t) = self.w.tex.get(417) {
                let y = if s.save { 0.0 } else { 76.0 };
                pen.spr(out, &t, 16, Some([0.0, y, 512.0, y + 76.0]), 0.0, 0.0);
            }
            self.wg_draw(out, &pen, if (26..32).contains(&s.st) { 10 } else { 0 }, 0.0, 0.0);
            // jp_E1BFC(2, 1): quad-out from 120 below
            let y = 120.0 * (1.0 - a.clamp(0.0, 1.0)).powi(2) + 418.13;
            for (k, x) in TAB_X.iter().enumerate() {
                self.w.objs.place(TAB_OBJ + k as i32, *x, y, a);
            }
            let tex = &self.w.tex;
            self.w.objs.draw(&super::Pen::default(), 2, out, &|id| tex.get(id));
        }
        let s = &self.w.save;
        // the line's frame (save_win), dropping in by (1 - a)^2 * 40
        let pane_y = (1.0 - a) * (1.0 - a) * 40.0 + 326.87;
        pen.alpha(a);
        if let Some(t) = self.w.tex.get(414) {
            let src = if s.save { [40.0, 16.0, 1520.0, 240.0] } else { [40.0, 256.0, 1520.0, 480.0] };
            pen.spr(out, &t, 16, Some(src), 73.333, pane_y);
        }
        if s.anim <= 0.0 {
            return;
        }
        let (st, anim, dir) = (s.st, s.anim, s.dir);
        let shown = s.shown;
        let drag = s.drag[0] as f32 * 0.025;
        let newest = self.file.continue_cursor();
        // (k picks the column of one of SLIDE's rows)
        #[allow(clippy::needless_range_loop)]
        for k in 0..20usize {
            let (col, row) = ((k % 5) as f32, (k / 5) as f32);
            let mut dx = 0.0;
            let mut dy = 0.0;
            match st {
                0 | 1 => {
                    let d = 0.45 - 0.15 * row + 0.1 * col;
                    dy = (d - anim + 1.0).max(0.0).powi(2) * -320.0;
                }
                3 | 27 => {
                    let d = SLIDE[usize::from(dir > 0.0)][k];
                    dx = dir * (d - anim + 1.0).max(0.0).powi(2) * 320.0;
                }
                4 | 28 => {
                    let d = SLIDE[if dir > 0.0 { 2 } else { 3 }][k];
                    dx = dir * (d - anim + 1.0).max(0.0).powi(2) * -320.0;
                }
                5 => {
                    let d = 0.85 - 0.15 * row - 0.1 * col;
                    dy = (d - anim + 1.0).clamp(0.0, 1.0).powi(4) * -320.0;
                }
                _ => {}
            }
            let x = drag + 101.33 * col + 74.667 + dx;
            let y = 76.326 * row + 34.015 + dy;
            let n = 20 * shown as usize + k;
            // Slot records include variables, layers and the rewind snapshot.
            // Drawing only borrows them; never clone twenty records per frame.
            let data = self.slots.get(n).and_then(Option::as_ref);
            pen.alpha(a);
            let thumb = self.w.tex.get(416);
            match (data, self.ws_used(n)) {
                (Some(d), true) => match game.preview(n, d) {
                    Some(t) => {
                        let k = [W / 640.0, H / 448.0];
                        out.push(Quad::rect(t, [x * k[0], y * k[1], 100.0 * 0.827 * k[0], 53.0 * 1.092 * k[1]], [1.0, 1.0, 1.0, a]));
                        self.w.save.drawn[n] = true;
                    }
                    None => {
                        if let Some(t) = &thumb {
                            pen.spr(out, t, 16, Some([252.0, 144.0, 500.0, 284.0]), x, y);
                        }
                    }
                },
                _ => {
                    let y0 = if self.w.save.drawn[n] { 144.0 } else { 0.0 };
                    if let Some(t) = &thumb {
                        pen.spr(out, t, 16, Some([0.0, y0, 248.0, y0 + 140.0]), x, y);
                    }
                }
            }
            let s = &self.w.save;
            let copying = (26..32).contains(&st);
            let lit = if copying {
                shown == s.src.0 && k as i32 == s.src.1
            } else {
                k as i32 == s.col + 5 * s.row && !s.tabrow
            };
            let frame = if lit { [16.0, 16.0, 272.0, 186.0] } else { [288.0, 16.0, 544.0, 186.0] };
            if let Some(t) = self.w.tex.get(415) {
                pen.spr(out, &t, 16, Some(frame), x - 1.3333, y - 10.785);
            }
            if let Some(d) = data.filter(|_| self.ws_used(n)) {
                if (shown, k as i32) == (i32::from(newest.0), i32::from(newest.1)) {
                    let mut p = pen;
                    p.scale(SX, 0.4144);
                    if let Some(t) = self.w.tex.get(410) {
                        p.spr(out, &t, 16, Some([0.0, 0.0, 80.0, 20.0]), x, y - 9.5407);
                    }
                }
                if rec_var(d, 24) == 1 {
                    let i = scene_icon(d.scene) as f32;
                    let mut p = pen;
                    p.scale(0.16667, 0.20741);
                    if let Some(t) = self.w.tex.get(418) {
                        p.spr(out, &t, 16, Some([56.0 * i + 8.0, 16.0, 56.0 * i + 53.0, 61.0]), x + 71.0, y - 9.1259);
                    }
                }
            }
            if copying {
                let s = &self.w.save;
                if let Some(t) = self.w.tex.get(407) {
                    if shown == s.src.0 && k as i32 == s.src.1 {
                        pen.spr(out, &t, 16, Some([16.0, 16.0, 272.0, 186.0]), x - 1.3333, y - 10.785);
                    }
                    if k as i32 == s.col + 5 * s.row {
                        pen.spr(out, &t, 16, Some([288.0, 16.0, 544.0, 186.0]), x - 1.3333, y - 10.785);
                    }
                }
            }
            self.w.objs.place(SLOT_OBJ + k as i32, x, y, a);
        }
        if st != 5 && a > 0.0 && st >= 2 {
            self.ws_draw_line(game, r, out, pane_y, a);
        }
    }

    /// sub_2E220's pattern for the save screen: BG_save / BG_load (shader noalpha) into surface 1, kusa1 /
    /// kusa2 (408 / 409) into surface 0 added onto it (sub_81C20(1)), surface 1 drawn at alpha a (jp_B1580).
    fn ws_backdrop(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, a: f32) {
        #[cfg(target_os = "vita")]
        let surfaces = &mut self.w.backdrop_rt;
        #[cfg(not(target_os = "vita"))]
        let surfaces = &mut self.w.save.rt;
        #[cfg(target_os = "vita")]
        if surfaces.is_none() {
            let pair = (|| -> anyhow::Result<_> { Ok([
                game.vita_scene_target(r, W as u32, H as u32, "save surface 0")?.tex,
                game.vita_scene_target(r, W as u32, H as u32, "save surface 1")?.tex]) })();
            let Ok(pair) = pair else { return }; // Saving/loading does not require the animated backdrop.
            *surfaces = Some(pair);
        }
        #[cfg(target_os = "vita")]
        let [t0, t1] = surfaces.as_ref().unwrap();
        #[cfg(not(target_os = "vita"))]
        let [t0, t1] = surfaces.get_or_insert_with(|| {
            let mk = |l| r.create_scene_target(W as u32, H as u32, l).tex;
            [mk("save surface 0"), mk("save surface 1")]
        });
        let (t0, t1) = (t0.clone(), t1.clone());
        let mut pen = super::Pen::default();
        pen.scale(SX, SY);
        let mut s1 = Vec::new();
        if let Some(t) = self.w.tex.get(if self.w.save.save { 406 } else { 405 }) {
            pen.spr(&mut s1, &t, 16, None, 0.0, 0.0);
            if let Some(q) = s1.last_mut() {
                q.blend = Blend::Opaque;
                q.shade = Shade::NoAlpha;
            }
        }
        let mut s0 = Vec::new();
        if let Some(t) = self.w.tex.get(408) {
            pen.spr(&mut s0, &t, 18, None, 640.0, -47.289);
        }
        if let Some(t) = self.w.tex.get(409) {
            pen.spr(&mut s0, &t, 24, None, 640.0, 448.0);
        }
        for q in &mut s0 {
            q.blend = Blend::Over;
        }
        let mut add = Quad::rect(t0.clone(), [0.0, 0.0, W, H], [1.0; 4]);
        add.blend = Blend::Add;
        s1.push(add);
        game.ui_passes.push((RenderTarget { tex: t0 }, Some([0.0; 4]), s0));
        game.ui_passes.push((RenderTarget { tex: t1.clone() }, Some([0.0; 4]), s1));
        out.push(Quad::rect(t1, [0.0, 0.0, W, H], [1.0, 1.0, 1.0, a]));
    }

    /// sub_31F30's line and its draw (sub_302A0): "No.###" (text slot 44, x 80), the chapter (45, x 136.67:
    /// SYSMES 213 + F023, "データ無し" for a free record), the date (47, right to x 553.33), the record's text
    /// (48, from x 84.667, 33.185 lower, two lines) and the archive icon for a TeaTime replay record (F024).
    fn ws_draw_line(&mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, pane_y: f32, a: f32) {
        let (page, slot) = self.w.save.line;
        if slot < 0 {
            return;
        }
        let n = (20 * page + slot) as usize;
        let y = pane_y + if crate::text::english() { 2.9037 } else { 1.6593 };
        let white = [1.0, 1.0, 1.0, a];
        let size = [0.42, 0.414];
        self.wtext(game, r, out, &format!("No.{:03}", n + 1), 0xB11020, size, 4.0, 80.0, y, 0, white);
        let chapter = self.slots.get(n).and_then(Option::as_ref)
            .filter(|_| self.ws_used(n)).map(|d| rec_var(d, 23) & 0xFF);
        let Some(ch) = chapter else {
            // text slot 39, "データ無し"
            let line = self.sysmes.get(188).cloned().unwrap_or_default();
            self.wtext(game, r, out, &line, 0xB11020, size, 4.0, 136.67, y, 0, white);
            return;
        };
        if ch >= 1 {
            let line = self.sysmes.get(213 + ch as usize).cloned().unwrap_or_default();
            self.wtext(game, r, out, &line, 0xB01020, size, 4.0, 136.67, y, 0, white);
        }
        if let Some(rec) = self.file.record(n) {
            let [yy, mo, dd, hh, mi, ss] = savedata::record_date(rec);
            let date = format!("{yy:04}/{mo:02}/{dd:02} {hh:02}:{mi:02}:{ss:02}");
            self.wtext(game, r, out, &date, 0xB11020, size, 4.0, 549.33, y, 2, white);
        }
        // Reborrow after the mutable text helpers above; no full SaveData copy.
        let Some(d) = self.slots.get(n).and_then(Option::as_ref) else { return };
        // the record's text with its ruby, italics and colours (jp_53CF0 only resolves the `$` lines) in slot 48:
        // F1105 (the window type) below 3 (sub_1ADD0) the message font at 0.6512 x 0.6808, else flag 0x100000
        // (the bubble font, line spacing 8 px less) at 0.6512; line spacing 6 + 14, 394 units wide, two lines
        // (jp_A8330(48, 2)). Switch: the ruby over 詠 in 5A_12's note.
        let bubble = rec_var(&d, 1105) >= 3;
        let z = game.slot_z(48, &d.preview);
        let slot = crate::game::WitchSlot {
            width: 394.0,
            kx: 0.6512,
            ky: if bubble { 0.6512 } else { 0.6808 },
            a8: 6.0,
            bubble,
            z,
            ruby_y: crate::text::RUBY_Y,
        };
        let placed = game.witch_slot_layout(&d.preview, 84.667, pane_y + 33.185, slot);
        out.extend(game.witch_slot_quads(r, &placed, bubble || z.is_some(), &d.preview, 2, a));
        if rec_var(&d, 24) == 1 {
            let i = scene_icon(d.scene) as f32;
            let mut p = super::Pen::default();
            p.scale(0.23333, 0.29037);
            p.alpha(a);
            if let Some(t) = self.w.tex.get(418) {
                p.spr(out, &t, 16, Some([56.0 * i + 8.0, 16.0, 56.0 * i + 53.0, 61.0]), 125.33, pane_y + 4.1481);
            }
        }
    }
}

impl Ui {
    /// A text slot line (jp_AB4D0 / jp_AB710) at (x, y) of 640x448: glyphs 60 sx x 60 sy px, `spacing` after
    /// each; the slot's flags 0x100000 the shared system font (else the message font) and 0x10000 the free
    /// layout; `align` 0 left, 2 right (0x200), 1 centred (0x100).
    #[allow(clippy::too_many_arguments)]
    pub(in crate::ui) fn wtext(
        &mut self, game: &mut Game, r: &Renderer, out: &mut Vec<Quad>, s: &str, flags: u32, size: [f32; 2], spacing: f32, x: f32, y: f32,
        align: u8, color: [f32; 4],
    ) {
        let (sys, free) = (flags & 0x100000 != 0, flags & 0x10000 != 0);
        let px = [60.0 * size[0], 60.0 * size[1]];
        let base = y * H / 448.0 + ascent(sys, px[1]);
        let Some(font) = self.wfont(game, r, sys) else { return };
        let w = font.measure_slot_as(s, px, spacing, free);
        let x = x * W / 640.0 - [0.0, w / 2.0, w][usize::from(align.min(2))];
        font.draw_str_slot_as(r, out, s, px, spacing, [x, base], color, free);
    }

    fn wfont<'a>(&'a mut self, game: &'a mut Game, r: &Renderer, sys: bool) -> Option<&'a mut crate::text::FontAtlas> {
        if !sys {
            return Some(game.font_mut());
        }
        if self.font_sys.is_none() {
            self.font_sys = Self::sys_font(game, r);
        }
        self.font_sys.as_mut()
    }
}

/// The first baseline below a slot's y: the system font's (`SYS_ASCENT`) or the message font's.
fn ascent(sys: bool, h: f32) -> f32 {
    if sys {
        super::super::SYS_ASCENT * crate::text::glyph_scale() * h
    } else {
        let lay = crate::game::LAYOUT;
        crate::text::msg_ascent(lay.ascent * h / lay.size)
    }
}

/// jp_53F90: a byte variable of a record (0 when unset).
fn rec_var(d: &crate::save::SaveData, n: usize) -> i32 {
    let key = format!("F{n:03}");
    d.vars.iter().find(|(k, _)| *k == key).map_or(0, |v| v.1)
}

/// sub_35530
fn scene_icon(scene: usize) -> u8 {
    scene.checked_sub(1).and_then(|i| SCENE_ICON.get(i)).copied().unwrap_or(0)
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use super::super::{WIn, WObjs};

    fn setup(slot: i32, page: i32) -> (WSave, WObjs) {
        let mut s = WSave { slot, page, ..Default::default() };
        let mut o = WObjs::default();
        o.install(objtab::SAVE);
        for k in 0..20 { o.create(SLOT_OBJ + k, 2, 0, [-3, -2, 86, 62]); }
        o.group_flags(2, 1, 0);
        s.reopen_focus(&mut o.focus);
        (s, o)
    }

    fn pad_frame(s: &mut WSave, o: &mut WObjs) {
        o.update(&mut WIn::default());
        s.resolve_focus(o.hit, false, &mut o.focus);
    }

    #[test]
    fn grid_edges_transfer_visible_focus_to_page_tabs() {
        for page in 0..6 {
            for col in 0..5 {
                for (slot, key) in [(col, UP), (col + 15, DOWN)] {
                    let (mut s, mut o) = setup(slot, page);
                    assert!(s.move_grid(key, key, &mut o.focus));
                    for _ in 0..3 { pad_frame(&mut s, &mut o); }
                    assert!(s.tabrow);
                    assert_eq!((s.tab, o.focus), (page, TAB_OBJ + page));
                    assert_eq!(o.o[o.focus as usize].as_ref().unwrap().state & 1, 1);
                    // Returning from tabs restores the same column in either row.
                    for row in [0, 3] {
                        s.focus_slot(col + 5 * row, &mut o.focus);
                        pad_frame(&mut s, &mut o);
                        assert!(!s.tabrow);
                        assert_eq!((s.slot, s.col, s.row, o.focus),
                            (col + 5 * row, col, row, SLOT_OBJ + col + 5 * row));
                    }
                }
            }
        }
    }

    #[test]
    fn all_grid_directions_and_repeat_edges_preserve_selection() {
        for slot in 0..20 {
            for (key, expected) in [(LEFT, (slot + 19) % 20), (RIGHT, (slot + 1) % 20),
                (UP, slot - 5), (DOWN, slot + 5)] {
                let (mut s, mut o) = setup(slot, 2);
                s.move_grid(key, key, &mut o.focus);
                pad_frame(&mut s, &mut o);
                if (0..20).contains(&expected) {
                    assert!(!s.tabrow);
                    assert_eq!((s.slot, o.focus), (expected, SLOT_OBJ + expected));
                } else { assert!(s.tabrow); assert_eq!(o.focus, TAB_OBJ + 2); }
            }
        }
        for (slot, key) in [(0, UP), (19, DOWN), (0, LEFT), (19, RIGHT)] {
            let (mut s, mut o) = setup(slot, 0);
            assert!(!s.move_grid(0, key, &mut o.focus));
            assert!(!s.tabrow);
            assert_eq!(s.slot, slot);
        }
    }

    #[test]
    fn synthetic_page_slide_hit_does_not_pull_pad_back() {
        let (mut s, mut o) = setup(17, 3);
        o.lock = 1;
        o.update(&mut WIn::default());
        o.lock = 0;
        assert_eq!(o.hit, SLOT_OBJ + 17);
        s.move_grid(DOWN, DOWN, &mut o.focus);
        pad_frame(&mut s, &mut o);
        assert!(s.tabrow);
        assert_eq!(o.focus, TAB_OBJ + 3);
        s.focus_slot(2, &mut o.focus);
        s.resolve_focus(TAB_OBJ + 3, false, &mut o.focus);
        assert!(!s.tabrow);
        assert_eq!(o.focus, SLOT_OBJ + 2);
    }

    #[test]
    fn real_pointer_overrides_tabs_and_slots_but_invalid_hit_preserves_focus() {
        let (mut s, mut o) = setup(0, 1);
        for tab in 0..6 {
            s.resolve_focus(TAB_OBJ + tab, true, &mut o.focus);
            assert!(s.tabrow);
            assert_eq!(o.focus, TAB_OBJ + tab);
            s.resolve_focus(-1, true, &mut o.focus);
            assert!(s.tabrow);
            for slot in 0..20 {
                s.resolve_focus(SLOT_OBJ + slot, true, &mut o.focus);
                assert!(!s.tabrow);
                assert_eq!((s.slot, o.focus), (slot, SLOT_OBJ + slot));
            }
        }
    }

    #[test]
    fn touch_release_keeps_its_hit_for_confirmation() {
        let (mut s, mut o) = setup(7, 1);
        let mut inp = WIn::default();
        // Simulate a hit established while the finger was down. Release has
        // active=false but still carries the valid tap and retained hit.
        o.hit = SLOT_OBJ + 7;
        inp.touch_down(0.0, 0.0);
        for _ in 0..8 { inp.begin(); }
        inp.touch_up();
        inp.begin();
        assert!(!inp.active);
        assert_ne!(inp.touch & 0x2002, 0);
        o.update(&mut inp);
        s.resolve_focus(o.hit, inp.active || inp.touch & 0x2002 != 0, &mut o.focus);
        assert_eq!((s.slot, o.hit, o.focus), (7, SLOT_OBJ + 7, SLOT_OBJ + 7));
        assert!(!s.tabrow);
    }

    #[test]
    fn reopen_save_load_and_copy_keep_slot_and_source_without_hidden_grid() {
        for saving in [false, true] {
            for st in [2, 26] {
                let (mut s, mut o) = setup(17, 4);
                s.save = saving;
                s.st = st;
                s.src = (1, 3);
                s.focus_tab(2, &mut o.focus);
                // A dialog/title/menu may own the shared focus before reopening.
                o.focus = 170;
                s.reopen_focus(&mut o.focus);
                pad_frame(&mut s, &mut o);
                assert!(!s.tabrow);
                assert_eq!((s.page, s.tab, s.slot, o.focus), (4, 4, 17, 25));
                assert_eq!(s.src, (1, 3));
                assert_eq!(s.st, st);
            }
        }
    }
}
