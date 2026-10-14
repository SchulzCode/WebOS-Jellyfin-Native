//! Search: an on-screen keyboard (works with the plain remote) and live results.
use super::*;
use crate::app::PlaySpec;
use crate::jf::Item;
use crate::net::Mailbox;
use std::time::{Duration, Instant};

const KEYS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ1234567890";
const KC: usize = 6;
const RC: usize = 5;
const RW: f32 = 184.0;
const RH: f32 = 276.0;

enum Msg { Results(u64, Vec<Item>) }

pub struct Search {
    q: String,
    mb: Mailbox<Msg>,
    results: Vec<Item>,
    gen: u64,
    dirty_at: Option<Instant>,
    in_results: bool,
    kidx: usize,
    ridx: usize,
    focus: Vec<Anim>,
    sy: Anim,
    searching: bool,
    appear: Anim,
}

fn keys() -> Vec<String> {
    let mut v: Vec<String> = KEYS.chars().map(|c| c.to_string()).collect();
    v.push("Space".into());
    v.push("Delete".into());
    v.push("Clear".into());
    v
}

impl Search {
    pub fn new(_cx: &mut Cx) -> Search {
        let mut s = Search { q: String::new(), mb: Mailbox::new(), results: vec![], gen: 0, dirty_at: None, in_results: false, kidx: 0, ridx: 0,
            focus: vec![], sy: Anim::new(0.0, 9.0), searching: false, appear: Anim::new(0.0, 4.0) };
        s.appear.set(1.0);
        s
    }
    fn edit(&mut self, f: impl FnOnce(&mut String)) { f(&mut self.q); self.dirty_at = Some(Instant::now()); }
    fn press(&mut self) {
        let k = keys();
        match k[self.kidx].as_str() {
            "Space" => self.edit(|q| q.push(' ')),
            "Delete" => self.edit(|q| { q.pop(); }),
            "Clear" => self.edit(|q| q.clear()),
            s => { let c = s.to_string(); self.edit(|q| q.push_str(&c)); }
        }
    }
}

impl Screen for Search {
    fn name(&self) -> &'static str { "search" }

    fn key(&mut self, k: Key, cx: &mut Cx) -> Action {
        let nk = keys().len();
        match k {
            Key::Back => { if self.in_results { self.in_results = false; } else { return Action::Pop; } }
            Key::Exit => return Action::Exit,
            Key::Char(c) => { self.edit(|q| q.push(c)); }
            Key::Backspace => { self.edit(|q| { q.pop(); }); }
            Key::Up => {
                if self.in_results { if self.ridx >= RC { self.ridx -= RC; } }
                else if self.kidx >= KC { self.kidx -= KC; }
            }
            Key::Down => {
                if self.in_results { if self.ridx + RC < self.results.len() { self.ridx += RC; } }
                else if self.kidx + KC < nk { self.kidx += KC; } else { self.kidx = nk - 1; }
            }
            Key::Left => {
                if self.in_results { if self.ridx % RC == 0 { self.in_results = false; } else { self.ridx -= 1; } }
                else if self.kidx % KC > 0 { self.kidx -= 1; }
            }
            Key::Right => {
                if self.in_results { if self.ridx % RC < RC - 1 && self.ridx + 1 < self.results.len() { self.ridx += 1; } }
                else if self.kidx % KC < KC - 1 && self.kidx + 1 < nk { self.kidx += 1; }
                else if !self.results.is_empty() { self.in_results = true; self.ridx = 0; }
            }
            Key::Ok => {
                if self.in_results { if let Some(it) = self.results.get(self.ridx) { return Action::Push(Box::new(super::detail::Detail::new(it.clone(), cx))); } }
                else { self.press(); }
            }
            Key::Play => { if self.in_results { if let Some(it) = self.results.get(self.ridx) { if !it.is_folder_like() { return Action::Play(PlaySpec::resume(it.clone())); } } } }
            _ => {}
        }
        Action::None
    }

    fn hover(&mut self, h: Hot) -> bool {
        let ch = if h.row == 0 { self.in_results || self.kidx != h.col as usize } else { !self.in_results || self.ridx != h.col as usize };
        if h.row == 0 { self.in_results = false; self.kidx = h.col as usize; } else { self.in_results = true; self.ridx = h.col as usize; }
        ch
    }

    fn update(&mut self, cx: &mut Cx) -> bool {
        for m in self.mb.drain() {
            let Msg::Results(gen, v) = m;
            if gen == self.gen { self.searching = false; self.focus = v.iter().map(|_| Anim::new(0.0, 14.0)).collect(); self.results = v; self.ridx = 0; }
        }
        if let Some(at) = self.dirty_at {
            if at.elapsed() > Duration::from_millis(350) {
                self.dirty_at = None;
                self.gen += 1;
                let q = self.q.trim().to_string();
                if q.len() >= 2 {
                    if let Some(c) = cx.client.clone() {
                        self.searching = true;
                        let (tx, gen) = (self.mb.tx.clone(), self.gen);
                        cx.pool.spawn(move || { let v = c.search(&q).unwrap_or_default(); let _ = tx.send(Msg::Results(gen, v)); crate::net::wake(); });
                    }
                } else { self.results.clear(); self.focus.clear(); }
            }
        }
        let mut busy = self.dirty_at.is_some() || self.searching;
        let row = self.ridx / RC;
        self.sy.set((row as f32 - 1.0).max(0.0) * (RH + 92.0));
        busy |= self.sy.step(cx.dt) | self.appear.step(cx.dt);
        for (i, f) in self.focus.iter_mut().enumerate() { f.set(if self.in_results && i == self.ridx { 1.0 } else { 0.0 }); busy |= f.step(cx.dt); }
        busy
    }

    fn draw(&mut self, g: &mut Gfx, cx: &mut Cx) {
        let Some(c) = cx.client.clone() else { return };
        background(g, cx.t);
        g.alpha = smooth(self.appear.v);
        g.text_t(PAD, 52.0, 56.0, W::Bold, TEXT, "Search", -1.0);
        // query field
        let field = Rect::new(PAD, 150.0, 552.0, 72.0);
        g.rect(field, SURFACE_HI, 18.0);
        draw_icon(g, Icon::Search, field.x + 22.0, field.y + 23.0, 26.0, MUTED);
        let shown = if self.q.is_empty() { "Titles, shows, episodes".to_string() } else { self.q.clone() };
        let caret = if (cx.t * 1.6).fract() < 0.55 && !self.in_results { "|" } else { "" };
        let fitted = g.text.ellipsize(&shown, 28.0, W::Medium, 450.0);
        g.text(field.x + 66.0, field.y + 20.0, 28.0, W::Medium, if self.q.is_empty() { FAINT } else { TEXT }, &format!("{fitted}{}", if self.q.is_empty() { "" } else { caret }));
        // keyboard
        let ks = keys();
        let (kw, kh, kg) = (84.0, 70.0, 9.6);
        for (i, k) in ks.iter().enumerate() {
            let (r, col) = (i / KC, i % KC);
            let (w, x) = if i >= 36 { let wide = (552.0 - 2.0 * kg) / 3.0; (wide, PAD + (i - 36) as f32 * (wide + kg)) } else { (kw, PAD + col as f32 * (kw + kg)) };
            let rect = Rect::new(x, 250.0 + r as f32 * (kh + kg), w, kh);
            let f = !self.in_results && i == self.kidx;
            if f { g.shadow(rect.offset(0.0, 8.0), Color(0.0, 0.0, 0.0, 0.5), 14.0, 20.0); }
            g.rect(rect, if f { Color(1.0, 1.0, 1.0, 1.0) } else { SURFACE }, 14.0);
            g.text_c(rect.x + w / 2.0, rect.y + 20.0, if k.len() > 1 { 22.0 } else { 27.0 }, W::Semi, if f { BG } else { TEXT }, k);
            cx.hot.push((rect, Hot { row: 0, col: i as i32 }));
        }
        // results
        let rx = 780.0;
        g.push_clip(Rect::new(rx - 40.0, 130.0, 1920.0 - rx + 40.0, 950.0));
        if self.results.is_empty() {
            let msg = if self.searching { "" } else if self.q.trim().len() >= 2 && self.dirty_at.is_none() { "Nothing found" } else { "Start typing to search your libraries" };
            g.text(rx, 180.0, 24.0, W::Regular, MUTED, msg);
            if self.searching { spinner(g, rx + 30.0, 200.0, 18.0, cx.t, MUTED); }
        }
        for (i, it) in self.results.clone().iter().enumerate() {
            let (r, col) = (i / RC, i % RC);
            let y = 160.0 + r as f32 * (RH + 92.0) - self.sy.v;
            if y > 1100.0 { break; }
            if y + RH < 100.0 { continue; }
            let rect = Rect::new(rx + col as f32 * (RW + 34.0), y, RW, RH);
            poster_card(g, cx, &c, it, rect, self.focus.get(i).map_or(0.0, |f| f.v), true);
            cx.hot.push((rect, Hot { row: 1, col: i as i32 }));
        }
        g.pop_clip();
        g.alpha = 1.0;
    }
}
