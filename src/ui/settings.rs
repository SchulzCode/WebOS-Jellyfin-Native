//! Settings: playback preferences + account. Preferences persist in /media/developer/jfnative/prefs.json.
use super::*;
use crate::app::{prefs, set_prefs, Prefs};

pub struct Settings { idx: usize, confirm_signout: bool, appear: Anim }

const ROWS: usize = 7;

impl Settings {
    pub fn new(_cx: &mut Cx) -> Settings { let mut s = Settings { idx: 0, confirm_signout: false, appear: Anim::new(0.0, 4.0) }; s.appear.set(1.0); s }

    fn rows(p: &Prefs) -> Vec<(&'static str, String)> {
        vec![
            ("Subtitle size", ["Small", "Medium", "Large"][p.sub_size.min(2)].to_string()),
            ("Subtitles", if p.subs_auto { "As set in your Jellyfin account".into() } else { "Off unless I pick them".into() }),
            ("Audio language", if p.audio_lang.is_empty() { "As set in your Jellyfin account".into() } else { p.audio_lang.clone() }),
            ("Remember my audio and subtitle choice per series", if p.remember_tracks { "On".into() } else { "Off".into() }),
            ("Play the next episode automatically", if p.autoplay { "On".into() } else { "Off".into() }),
            ("Show technical info in the player", if p.stats { "On".into() } else { "Off".into() }),
            ("Sign out", String::new()),
        ]
    }

    fn change(&mut self, d: i32) {
        let mut p = prefs();
        match self.idx {
            0 => p.sub_size = (p.sub_size as i32 + d).rem_euclid(3) as usize,
            1 => p.subs_auto = !p.subs_auto,
            2 => {
                const L: [&str; 6] = ["", "eng", "ger", "jpn", "fre", "spa"];
                let i = L.iter().position(|l| *l == p.audio_lang).unwrap_or(0) as i32;
                p.audio_lang = L[(i + d).rem_euclid(L.len() as i32) as usize].to_string();
            }
            3 => p.remember_tracks = !p.remember_tracks,
            4 => p.autoplay = !p.autoplay,
            5 => p.stats = !p.stats,
            _ => return,
        }
        set_prefs(p);
    }
}

impl Screen for Settings {
    fn name(&self) -> &'static str { "settings" }

    fn key(&mut self, k: Key, _cx: &mut Cx) -> Action {
        match k {
            Key::Back => { if self.confirm_signout { self.confirm_signout = false; } else { return Action::Pop; } }
            Key::Exit => return Action::Exit,
            Key::Up => { self.idx = self.idx.saturating_sub(1); self.confirm_signout = false; }
            Key::Down => { self.idx = (self.idx + 1).min(ROWS - 1); self.confirm_signout = false; }
            Key::Left => self.change(-1),
            Key::Right => self.change(1),
            Key::Ok => {
                if self.idx == ROWS - 1 {
                    if self.confirm_signout { return Action::SignOut; }
                    self.confirm_signout = true;
                } else { self.change(1); }
            }
            _ => {}
        }
        Action::None
    }

    fn hover(&mut self, h: Hot) -> bool { let ch = self.idx != h.col as usize; self.idx = h.col as usize; ch }

    fn update(&mut self, cx: &mut Cx) -> bool { self.appear.step(cx.dt) }

    fn draw(&mut self, g: &mut Gfx, cx: &mut Cx) {
        background(g, cx.t);
        g.alpha = smooth(self.appear.v);
        g.text_t(PAD, 52.0, 56.0, W::Bold, TEXT, "Settings", -1.0);
        let p = prefs();
        for (i, (name, val)) in Self::rows(&p).iter().enumerate() {
            let r = Rect::new(PAD, 170.0 + i as f32 * 96.0, 1100.0, 80.0);
            let f = i == self.idx;
            let danger = i == ROWS - 1;
            g.rect(r, if f { Color(1.0, 1.0, 1.0, 1.0) } else { SURFACE }, 18.0);
            let ink = if f { BG } else if danger { Color::hex(0xff9b9b, 1.0) } else { TEXT };
            let label = if danger && self.confirm_signout { "Press OK again to sign out" } else { name };
            g.text(r.x + 32.0, r.y + 25.0, 26.0, W::Semi, ink, label);
            if !val.is_empty() {
                g.text_r(r.right() - 72.0, r.y + 27.0, 23.0, W::Medium, if f { Color::hex(0x07080c, 0.7) } else { MUTED }, val);
                if f {
                    g.tri([(r.right() - 52.0, r.y + 30.0), (r.right() - 52.0, r.y + 50.0), (r.right() - 38.0, r.y + 40.0)], BG.a(0.6));
                }
            }
            cx.hot.push((r, Hot { row: 0, col: i as i32 }));
        }
        if let Some(c) = cx.client {
            let info = format!("Signed in as {} on {}", c.s.user_name, c.s.server.replace("https://", ""));
            g.text(PAD, 170.0 + ROWS as f32 * 96.0 + 24.0, 21.0, W::Regular, MUTED, &info);
        }
        g.text(PAD, 1010.0, 19.0, W::Regular, FAINT, &format!("Jellyfin Native {} · native Rust client for webOS · plays on the TV's own decoder", crate::VERSION));
        // side panel: a short explanation of the focused setting
        let help = match self.idx {
            0 => "How large subtitles appear over the picture.",
            1 => "Turn subtitles on by themselves only when the file marks a track as default or forced (signs and songs in anime, foreign dialogue). You can always pick a track in the player with ▲.",
            2 => "The audio track to prefer when a file has several languages.",
            3 => "When an episode ends, the next one starts after a short countdown.",
            4 => "Show codec, resolution and how the file is played (direct or converted by the server).",
            _ => "Removes this TV's sign-in. You'll need a new Quick Connect code to sign in again.",
        };
        for (i, l) in g.text.wrap(help, 24.0, W::Regular, 560.0, 6).iter().enumerate() { g.text(1280.0, 190.0 + i as f32 * 36.0, 24.0, W::Regular, MUTED, l); }
        g.alpha = 1.0;
    }
}
