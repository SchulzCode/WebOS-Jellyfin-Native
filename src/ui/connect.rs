//! Sign-in. First the server: the ones found on the network (Jellyfin's UDP discovery), the last one used, or an
//! address typed with an on-screen keyboard. Then Quick Connect: show a code, approve it in any signed-in Jellyfin
//! client, done. No password is ever entered or stored.
use super::*;
use crate::jf::{self, Found, Session};
use crate::net::Mailbox;
use std::time::{Duration, Instant};

enum Msg { Found(Vec<Found>), Checked(Result<Found, String>), Code(Result<jf::QuickConnect, String>), Poll(Result<Option<Session>, String>) }

#[derive(PartialEq)]
enum Stage { Pick, Type, Code }

/// The keyboard for addresses: lower-case letters, digits, URL punctuation, and a few shortcuts.
const CHARS: &str = "abcdefghijklmnopqrstuvwxyz0123456789.:/-";
const KCOLS: usize = 10;
fn keys() -> Vec<String> {
    let mut v: Vec<String> = CHARS.chars().map(|c| c.to_string()).collect();
    for s in ["https://", "http://", ":8096", "Delete", "Clear", "Connect"] { v.push(s.into()); }
    v
}

pub struct Connect {
    stage: Stage,
    server: String,
    device: String,
    agent: ureq::Agent,
    mb: Mailbox<Msg>,
    /// servers to pick from (discovered + last used + built-in suggestion); the last entry is "enter an address"
    found: Vec<Found>,
    searching: bool,
    checking: bool,
    pidx: usize,
    typed: String,
    kidx: usize,
    code: String,
    secret: String,
    error: String,
    started: Instant,
    next_poll: Instant,
    polling: bool,
    appear: Anim,
    pending: Option<Session>,
}

impl Connect {
    pub fn new(agent: ureq::Agent, cx: &mut Cx) -> Connect {
        let mut c = Connect {
            stage: Stage::Pick, server: String::new(), device: jf::device_id(), agent, mb: Mailbox::new(), found: vec![], searching: false,
            checking: false, pidx: 0, typed: String::new(), kidx: 0, code: String::new(), secret: String::new(), error: String::new(),
            started: Instant::now(), next_poll: Instant::now(), polling: false, appear: Anim::new(0.0, 4.0), pending: None,
        };
        for url in [jf::last_server(), jf::BUILD_SERVER.map(String::from)].into_iter().flatten() {
            if !c.found.iter().any(|f| f.url == url) { c.found.push(Found { name: url.replace("https://", "").replace("http://", ""), url }); }
        }
        c.appear.set(1.0);
        c.discover(cx);
        c
    }

    fn discover(&mut self, cx: &mut Cx) {
        self.searching = true;
        let tx = self.mb.tx.clone();
        cx.pool.spawn(move || { let _ = tx.send(Msg::Found(jf::discover())); crate::net::wake(); });
    }

    fn check(&mut self, input: String, cx: &mut Cx) {
        self.checking = true;
        self.error.clear();
        let (tx, agent) = (self.mb.tx.clone(), self.agent.clone());
        cx.pool.spawn(move || { let _ = tx.send(Msg::Checked(jf::check_server(&agent, &input))); crate::net::wake(); });
    }

    fn initiate(&mut self, cx: &mut Cx) {
        self.stage = Stage::Code;
        self.code.clear(); self.secret.clear(); self.error.clear(); self.started = Instant::now();
        let (tx, agent, server, dev) = (self.mb.tx.clone(), self.agent.clone(), self.server.clone(), self.device.clone());
        cx.pool.spawn(move || { let _ = tx.send(Msg::Code(jf::qc_initiate(&agent, &server, &dev))); crate::net::wake(); });
    }

    fn press(&mut self, cx: &mut Cx) {
        let k = keys();
        match k[self.kidx].as_str() {
            "Delete" => { self.typed.pop(); }
            "Clear" => self.typed.clear(),
            "Connect" => { let t = self.typed.clone(); self.check(t, cx); }
            s => { if self.typed.len() < 120 { self.typed.push_str(s); } }
        }
    }
}

impl Screen for Connect {
    fn name(&self) -> &'static str { "connect" }

    fn poll_action(&mut self) -> Action {
        match self.pending.take() { Some(s) => Action::SignedIn(s), None => Action::None }
    }

    fn key(&mut self, k: Key, cx: &mut Cx) -> Action {
        match self.stage {
            Stage::Pick => match k {
                Key::Up => self.pidx = self.pidx.saturating_sub(1),
                Key::Down => self.pidx = (self.pidx + 1).min(self.found.len()),
                Key::Ok => {
                    if self.checking { return Action::None; }
                    match self.found.get(self.pidx).cloned() {
                        Some(f) => self.check(f.url, cx),
                        None => { self.stage = Stage::Type; self.error.clear(); }
                    }
                }
                Key::Back | Key::Exit => return Action::Exit,
                _ => {}
            },
            Stage::Type => {
                let n = keys().len();
                match k {
                    Key::Left => self.kidx = self.kidx.saturating_sub(1),
                    Key::Right => self.kidx = (self.kidx + 1).min(n - 1),
                    Key::Up => self.kidx = self.kidx.saturating_sub(KCOLS),
                    Key::Down => self.kidx = (self.kidx + KCOLS).min(n - 1),
                    Key::Ok => { if !self.checking { self.press(cx); } }
                    Key::Char(c) => { if self.typed.len() < 120 { self.typed.push(c); } }
                    Key::Backspace => { self.typed.pop(); }
                    Key::Back => { self.stage = Stage::Pick; self.error.clear(); }
                    Key::Exit => return Action::Exit,
                    _ => {}
                }
            }
            Stage::Code => match k {
                Key::Ok if !self.error.is_empty() || self.code.is_empty() => self.initiate(cx),
                // Back: pick another server
                Key::Back => { self.stage = Stage::Pick; self.secret.clear(); self.code.clear(); self.error.clear(); }
                Key::Exit => return Action::Exit,
                _ => {}
            },
        }
        Action::None
    }

    fn hover(&mut self, h: Hot) -> bool {
        let c = h.col as usize;
        let changed = match (h.row, &self.stage) {
            (1, Stage::Pick) => { let ch = self.pidx != c; self.pidx = c; ch }
            (2, Stage::Type) => { let ch = self.kidx != c; self.kidx = c; ch }
            _ => false,
        };
        changed
    }

    fn update(&mut self, cx: &mut Cx) -> bool {
        let mut out = None;
        for m in self.mb.drain() {
            match m {
                Msg::Found(v) => {
                    self.searching = false;
                    for f in v { if !self.found.iter().any(|x| x.url == f.url) { self.found.push(f); } }
                }
                Msg::Checked(r) => {
                    self.checking = false;
                    match r {
                        Ok(f) => { crate::log!("server ok: {} ({})", f.url, f.name); self.server = f.url; self.initiate(cx); }
                        Err(e) => self.error = e,
                    }
                }
                Msg::Code(Ok(q)) => { crate::log!("quick connect code {}", q.code); self.code = q.code; self.secret = q.secret; self.next_poll = Instant::now() + Duration::from_secs(3); }
                Msg::Code(Err(e)) => { self.error = e; }
                Msg::Poll(r) => {
                    self.polling = false;
                    match r { Ok(Some(s)) => out = Some(s), Ok(None) => {}, Err(e) => crate::log!("qc poll: {e}") }
                }
            }
        }
        if let Some(s) = out { self.pending = Some(s); }
        if self.stage == Stage::Code && !self.secret.is_empty() && !self.polling && Instant::now() >= self.next_poll && self.pending.is_none() {
            if self.started.elapsed() > Duration::from_secs(8 * 60) {
                self.error = "The code expired.".into(); self.secret.clear();
            } else {
                self.polling = true;
                self.next_poll = Instant::now() + Duration::from_secs(3);
                let (tx, agent, server, dev, secret) = (self.mb.tx.clone(), self.agent.clone(), self.server.clone(), self.device.clone(), self.secret.clone());
                cx.pool.spawn(move || { let _ = tx.send(Msg::Poll(jf::qc_poll(&agent, &server, &dev, &secret))); crate::net::wake(); });
            }
        }
        self.appear.step(cx.dt) | true // the spinner animates
    }

    fn draw(&mut self, g: &mut Gfx, cx: &mut Cx) {
        background(g, cx.t);
        let a = smooth(self.appear.v);
        g.alpha = a;
        let x = 220.0;
        let y0 = 250.0 + (1.0 - a) * 20.0;
        pill(g, x, y0, "Jellyfin", ACCENT);
        let err_color = Color::hex(0xff9b9b, 1.0);
        match self.stage {
            Stage::Pick | Stage::Type => {
                g.text_t(x, y0 + 64.0, 76.0, W::Bold, TEXT, "Your Jellyfin server", -1.6);
                let help = if self.stage == Stage::Pick { "Pick your server, or enter its address - for example 192.168.1.20:8096 or jellyfin.example.com." }
                    else { "Type the address with the keyboard. https and the port 8096 are tried automatically." };
                for (i, l) in g.text.wrap(help, 26.0, W::Regular, 760.0, 3).iter().enumerate() {
                    g.text(x, y0 + 176.0 + i as f32 * 38.0, 26.0, W::Regular, MUTED, l);
                }
                if !self.error.is_empty() {
                    for (i, l) in g.text.wrap(&self.error, 22.0, W::Regular, 760.0, 3).iter().enumerate() { g.text(x, y0 + 300.0 + i as f32 * 32.0, 22.0, W::Regular, err_color, l); }
                }
                if self.checking { spinner(g, x + 14.0, y0 + 420.0, 14.0, cx.t, MUTED); g.text(x + 44.0, y0 + 407.0, 22.0, W::Regular, MUTED, "Checking the server…"); }
            }
            Stage::Code => {
                g.text_t(x, y0 + 64.0, 76.0, W::Bold, TEXT, "Connect your Jellyfin", -1.6);
                let help = "On your phone or computer, open Jellyfin, choose your profile picture → Quick Connect, and enter this code.";
                for (i, l) in g.text.wrap(help, 26.0, W::Regular, 760.0, 3).iter().enumerate() {
                    g.text(x, y0 + 176.0 + i as f32 * 38.0, 26.0, W::Regular, MUTED, l);
                }
                g.text(x, y0 + 312.0, 20.0, W::Regular, FAINT, &format!("{}   ·   Back: another server", self.server.replace("https://", "").replace("http://", "")));
            }
        }

        match self.stage {
            Stage::Pick => {
                let mut y = 300.0;
                for i in 0..=self.found.len() {
                    let f = i == self.pidx;
                    let r = Rect::new(1080.0, y, 640.0, 84.0);
                    g.rect(r, if f { Color(1.0, 1.0, 1.0, 1.0) } else { SURFACE }, 20.0);
                    let ink = if f { BG } else { TEXT };
                    match self.found.get(i) {
                        Some(s) => {
                            let n = g.text.ellipsize(&s.name, 26.0, W::Semi, 580.0);
                            g.text(r.x + 30.0, r.y + 14.0, 26.0, W::Semi, ink, &n);
                            let u = g.text.ellipsize(&s.url, 18.0, W::Regular, 580.0);
                            g.text(r.x + 30.0, r.y + 50.0, 18.0, W::Regular, if f { BG } else { MUTED }, &u);
                        }
                        None => { g.text(r.x + 30.0, r.y + 28.0, 26.0, W::Semi, ink, "Enter an address…"); }
                    }
                    cx.hot.push((r, Hot { row: 1, col: i as i32 }));
                    y += 100.0;
                    if y > 960.0 { break; }
                }
                if self.searching { spinner(g, 1094.0, y + 24.0, 12.0, cx.t, MUTED); g.text(1120.0, y + 12.0, 20.0, W::Regular, MUTED, "Looking for servers on your network…"); }
            }
            Stage::Type => {
                let field = Rect::new(1000.0, 250.0, 800.0, 80.0);
                g.rect(field, SURFACE_HI, 18.0);
                let shown = if self.typed.is_empty() { "jellyfin.example.com".to_string() } else { format!("{}|", self.typed) };
                let t = g.text.ellipsize(&shown, 30.0, W::Medium, field.w - 50.0);
                g.text(field.x + 26.0, field.y + 22.0, 30.0, W::Medium, if self.typed.is_empty() { FAINT } else { TEXT }, &t);
                let ks = keys();
                let (kw, kh, gap) = (72.0, 66.0, 8.0);
                let mut px = 1000.0;
                let mut py = 370.0;
                let mut col = 0;
                for (i, k) in ks.iter().enumerate() {
                    let wide = k.len() > 1;
                    let w = if wide { kw * 2.0 + gap } else { kw };
                    if col >= KCOLS || (wide && px + w > 1000.0 + KCOLS as f32 * (kw + gap)) { px = 1000.0; py += kh + gap; col = 0; }
                    let r = Rect::new(px, py, w, kh);
                    let f = i == self.kidx;
                    g.rect(r, if f { Color(1.0, 1.0, 1.0, 1.0) } else if k == "Connect" { ACCENT.a(0.35) } else { SURFACE }, 14.0);
                    g.text_c(r.x + r.w / 2.0, r.y + 18.0, if wide { 20.0 } else { 26.0 }, W::Semi, if f { BG } else { TEXT }, k);
                    cx.hot.push((r, Hot { row: 2, col: i as i32 }));
                    px += w + gap;
                    col += if wide { 2 } else { 1 };
                }
            }
            Stage::Code => {
                // the code, one character per tile
                let tile = Rect::new(1080.0, 330.0, 96.0, 128.0);
                if !self.code.is_empty() {
                    for (i, ch) in self.code.chars().enumerate() {
                        let r = tile.offset(i as f32 * 112.0 + if i >= 3 { 24.0 } else { 0.0 }, 0.0);
                        g.shadow(r.offset(0.0, 16.0), Color(0.0, 0.0, 0.0, 0.5), 20.0, 30.0);
                        g.rect(r, SURFACE_HI, 20.0);
                        g.text_c(r.x + r.w / 2.0, r.y + 22.0, 70.0, W::Semi, ACCENT, &ch.to_string());
                    }
                    if self.pending.is_none() {
                        spinner(g, 1110.0, 540.0, 14.0, cx.t, MUTED);
                        g.text(1142.0, 527.0, 22.0, W::Regular, MUTED, "Waiting for you to approve the code…");
                    } else {
                        g.text(1080.0, 527.0, 22.0, W::Semi, ACCENT, "Approved — signing in");
                    }
                } else if self.error.is_empty() {
                    spinner(g, 1290.0, 394.0, 22.0, cx.t, MUTED);
                }
                if !self.error.is_empty() {
                    let lines = g.text.wrap(&self.error, 24.0, W::Regular, 640.0, 3);
                    for (i, l) in lines.iter().enumerate() { g.text(1080.0, 340.0 + i as f32 * 34.0, 24.0, W::Regular, err_color, l); }
                    let r = button(g, 1080.0, 470.0, "Try again", Some(Icon::Restart), 1.0, true, 64.0);
                    cx.hot.push((r, Hot { row: 0, col: 0 }));
                }
            }
        }
        g.alpha = 1.0;
    }
}
