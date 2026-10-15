//! UI building blocks shared by every screen: theme, animation, remote keys, local time, widgets, icons.
pub mod connect;
pub mod detail;
pub mod home;
pub mod library;
pub mod player_ui;
pub mod search;
pub mod settings;

use crate::gfx::images::Images;
use crate::gfx::text::W;
use crate::gfx::{Color, Gfx, Rect};
use crate::jf::{Client, Item};
use crate::net::Pool;
use std::sync::Arc;

// ---- theme (Minimal Home's palette) ----
pub const BG: Color = Color::hex(0x07080c, 1.0);
pub const TEXT: Color = Color::hex(0xf3f5f9, 1.0);
pub const MUTED: Color = Color::hex(0xf3f5f9, 0.62);
pub const FAINT: Color = Color::hex(0xf3f5f9, 0.38);
pub const SURFACE: Color = Color(1.0, 1.0, 1.0, 0.06);
pub const SURFACE_HI: Color = Color(1.0, 1.0, 1.0, 0.11);
pub const ACCENT: Color = Color::hex(0xa99bff, 1.0);
pub const PAD: f32 = 96.0;

// ---- animation ----
/// A value that eases towards its target (critically damped-ish exponential approach).
#[derive(Clone, Copy, Debug)]
pub struct Anim { pub v: f32, pub to: f32, pub speed: f32 }
impl Anim {
    pub fn new(v: f32, speed: f32) -> Anim { Anim { v, to: v, speed } }
    pub fn set(&mut self, to: f32) { self.to = to; }
    pub fn snap(&mut self, v: f32) { self.v = v; self.to = v; }
    /// Advance; returns true while still moving.
    pub fn step(&mut self, dt: f32) -> bool {
        let d = self.to - self.v;
        if d.abs() < 0.0015 * (1.0 + self.to.abs().min(2000.0) / 10.0) { self.v = self.to; return false; }
        self.v += d * (1.0 - (-self.speed * dt).exp());
        true
    }
}

pub fn smooth(t: f32) -> f32 { let t = t.clamp(0.0, 1.0); t * t * (3.0 - 2.0 * t) }

// ---- remote keys ----
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key { Up, Down, Left, Right, Ok, Back, Play, Pause, Stop, Ff, Rw, Exit, Red, Green, Yellow, Blue, ChUp, ChDown, Info, Char(char), Backspace }

/// A held remote button: after 0.5 s LG's SDL sends it again every ~50 ms as a new key-down (repeat flag 0) with this
/// scancode (measured on this TV).
pub const REMOTE_REPEAT: i32 = 257;

pub fn map_key(scancode: i32, sym: i32) -> Option<Key> {
    // LG's SDL delivers the remote's buttons as scancode 1 ("unknown") with the real code in `sym`
    // (arrows 79-82, OK 40, Back 482, ...), measured on this TV; a held button repeats as scancode 257 (see REMOTE_REPEAT).
    // Keyboards send normal scancodes.
    let code = if scancode == 1 || scancode == REMOTE_REPEAT { sym } else { scancode };
    Some(match code {
        82 => Key::Up, 81 => Key::Down, 80 => Key::Left, 79 => Key::Right,
        40 | 88 => Key::Ok,
        482 | 41 => Key::Back,
        450 => Key::Play, 72 => Key::Pause, 120 | 260 => Key::Stop,
        451 => Key::Ff, 452 => Key::Rw,
        505 => Key::Exit,
        486 => Key::Red, 487 => Key::Green, 488 => Key::Yellow, 489 => Key::Blue,
        300 => Key::ChUp, 301 => Key::ChDown,
        457 => Key::Info,
        42 => Key::Backspace,
        _ => {
            if scancode != 1 && (32..127).contains(&sym) { return Some(Key::Char((sym as u8 as char).to_ascii_uppercase())); }
            return None;
        }
    })
}

// ---- local time (glibc reads the TV's zone) ----
#[repr(C)]
struct Tm { sec: i32, min: i32, hour: i32, mday: i32, mon: i32, year: i32, wday: i32, yday: i32, isdst: i32, gmtoff: i32, zone: *const i8 }
extern "C" { fn localtime_r(t: *const i32, tm: *mut Tm) -> *mut Tm; fn tzset(); }

pub struct Local { pub hour: i32, pub min: i32 }
pub fn local_time(offset_s: i64) -> Local {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) + offset_s;
    let t = now as i32;
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    unsafe { localtime_r(&t, &mut tm); }
    Local { hour: tm.hour, min: tm.min }
}
pub fn init_tz() { unsafe { tzset(); } }
pub fn hhmm(offset_s: i64) -> String { let l = local_time(offset_s); format!("{:02}:{:02}", l.hour, l.min) }

/// Where the Magic Remote pointer is (None after a key press: the system hides the cursor then).
static POINTER: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);
pub fn set_pointer(p: Option<(f32, f32)>) { *POINTER.lock().unwrap() = p; }
pub fn pointer_at() -> Option<(f32, f32)> { *POINTER.lock().unwrap() }

// ---- shared context ----
pub struct Cx<'a> {
    pub images: &'a mut Images,
    pub client: &'a Option<Client>,
    pub pool: &'a Arc<Pool>,
    pub dt: f32,
    pub t: f32,
    /// pointer hot-spots registered while drawing (rect, target), consumed by pointer input
    pub hot: &'a mut Vec<(Rect, Hot)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hot { pub row: i32, pub col: i32 }

pub enum Action {
    None,
    Push(Box<dyn Screen>),
    Pop,
    Play(crate::app::PlaySpec),
    SignedIn(crate::jf::Session),
    SignOut,
    Exit,
    Toast(String),
}

pub trait Screen {
    fn key(&mut self, k: Key, cx: &mut Cx) -> Action;
    /// pointer over a hot-spot: focus it; return true if focus changed
    fn hover(&mut self, _h: Hot) -> bool { false }
    /// advance animations / async results; return true if the screen needs another frame
    fn update(&mut self, cx: &mut Cx) -> bool;
    fn draw(&mut self, g: &mut Gfx, cx: &mut Cx);
    /// the screen became visible again (returning from a child or from playback)
    fn resumed(&mut self, _cx: &mut Cx) {}
    /// an action that came out of background work (e.g. sign-in approved)
    fn poll_action(&mut self) -> Action { Action::None }
    fn name(&self) -> &'static str;
}

// ---- widgets ----
pub fn background(g: &mut Gfx, _t: f32) {
    // the launcher's two soft colour washes. Drawn live they were huge blurred quads costing the GPU more than
    // the rest of the screen, so they are baked once into a small texture (they are pure blur, so 1/5 scale is
    // invisible) and stretched over the screen.
    if g.bg.is_none() {
        let (w, h, s) = (384usize, 216usize, 5.0f32);
        let blobs = [(Rect::new(-260.0, -420.0, 1400.0, 900.0), Color::hex(0x7868ff, 0.13), 450.0f32, 380.0f32),
                     (Rect::new(1250.0, 760.0, 1100.0, 700.0), Color::hex(0x4678ff, 0.06), 350.0, 320.0)];
        let mut px = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = ((x as f32 + 0.5) * s, (y as f32 + 0.5) * s);
                let (mut r, mut gg, mut b) = (BG.0, BG.1, BG.2);
                for (rc, c, rad, blur) in blobs {
                    // same rounded-box distance and smoothstep as the shader
                    let (hw, hh) = (rc.w / 2.0, rc.h / 2.0);
                    let rad = rad.min(hw).min(hh);
                    let (qx, qy) = ((sx - rc.x - hw).abs() - hw + rad, (sy - rc.y - hh).abs() - hh + rad);
                    let d = (qx.max(0.0).hypot(qy.max(0.0))) + qx.max(qy).min(0.0) - rad;
                    let t = ((d + blur) / (2.0 * blur)).clamp(0.0, 1.0);
                    let a = c.3 * (1.0 - t * t * (3.0 - 2.0 * t));
                    r = c.0 * a + r * (1.0 - a); gg = c.1 * a + gg * (1.0 - a); b = c.2 * a + b * (1.0 - a);
                }
                let i = (y * w + x) * 4;
                px[i] = (r * 255.0 + 0.5) as u8; px[i + 1] = (gg * 255.0 + 0.5) as u8; px[i + 2] = (b * 255.0 + 0.5) as u8; px[i + 3] = 255;
            }
        }
        g.bg = Some(unsafe { g.upload_rgba(w as u32, h as u32, &px) });
    }
    let t = g.bg.as_ref().unwrap() as *const crate::gfx::Texture;
    g.blit(unsafe { &*t }, Rect::new(0.0, 0.0, 1920.0, 1080.0));
}

/// Focus decoration around a card: soft shadow + violet glow + white ring.
pub fn focus_ring(g: &mut Gfx, r: Rect, radius: f32, f: f32) {
    if f <= 0.01 { return; }
    g.shadow(r.offset(0.0, 14.0), Color(0.0, 0.0, 0.0, 0.55 * f), radius, 34.0);
    g.shadow(r, Color::hex(0xa99bff, 0.30 * f), radius, 30.0);
    g.ring(r.inset(-3.0), Color(1.0, 1.0, 1.0, 0.92 * f), radius + 3.0, 3.0);
}

/// Image or placeholder with a gentle shimmer while loading.
pub fn picture(g: &mut Gfx, cx: &mut Cx, url: &str, r: Rect, radius: f32, initial: &str) {
    let img = cx.images.get(url).map(|(t, a)| (t as *const crate::gfx::Texture, a));
    // the placeholder only shows while the (opaque) picture is missing or fading in
    if img.map_or(true, |(_, a)| a < 1.0) { g.vgrad(r, Color::hex(0x1b1d2a, 1.0), Color::hex(0x11121a, 1.0), radius); }
    match img {
        Some((tex, a)) => {
            let tex = unsafe { &*tex };
            g.image(tex, r, radius, a, 0.5, 0.25);
        }
        None => {
            if !initial.is_empty() {
                let s = (r.h * 0.34).min(72.0);
                g.text_c(r.x + r.w / 2.0, r.y + r.h / 2.0 - s * 0.6, s, W::Semi, FAINT, initial);
            }
            if !url.is_empty() {
                let p = (cx.t * 0.9).fract();
                let x = r.x - r.w * 0.4 + p * r.w * 1.8;
                g.push_clip(r);
                g.hgrad(Rect::new(x, r.y, r.w * 0.4, r.h), Color(1.0, 1.0, 1.0, 0.0), Color(1.0, 1.0, 1.0, 0.035), 0.0);
                g.hgrad(Rect::new(x + r.w * 0.4, r.y, r.w * 0.4, r.h), Color(1.0, 1.0, 1.0, 0.035), Color(1.0, 1.0, 1.0, 0.0), 0.0);
                g.pop_clip();
            }
        }
    }
}

pub fn initial(s: &str) -> String { s.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default() }

/// Small rounded label like the launcher's pill ("12 MIN LEFT").
pub fn pill(g: &mut Gfx, x: f32, y: f32, label: &str, dot: Color) -> f32 {
    let size = 17.0;
    let tw = g.text.measure(&label.to_uppercase(), size, W::Semi, 1.4);
    let w = tw + 42.0;
    g.rect(Rect::new(x, y, w, 38.0), Color(1.0, 1.0, 1.0, 0.10), 19.0);
    g.rect(Rect::new(x + 15.0, y + 15.0, 8.0, 8.0), dot, 4.0);
    g.text_t(x + 31.0, y + 9.0, size, W::Semi, TEXT, &label.to_uppercase(), 1.4);
    w
}

pub fn label(g: &mut Gfx, x: f32, y: f32, s: &str) { g.text_t(x, y, 19.0, W::Semi, FAINT, &s.to_uppercase(), 1.9); }

/// A pill button (detail page actions, dialogs). Returns its rect.
#[allow(clippy::too_many_arguments)]
pub fn button(g: &mut Gfx, x: f32, y: f32, text: &str, icon: Option<Icon>, focus: f32, primary: bool, h: f32) -> Rect {
    let size = 24.0;
    let tw = if text.is_empty() { 0.0 } else { g.text.measure(text, size, W::Semi, 0.0) };
    let iw = if icon.is_some() { 26.0 + if text.is_empty() { 0.0 } else { 14.0 } } else { 0.0 };
    let w = if text.is_empty() { h } else { tw + iw + 64.0 };
    let r = Rect::new(x, y, w, h);
    let base = if primary { Color(1.0, 1.0, 1.0, 0.16) } else { SURFACE };
    let fill = base.lerp(Color(1.0, 1.0, 1.0, 1.0), focus);
    let ink = TEXT.lerp(Color::hex(0x07080c, 1.0), focus);
    if focus > 0.01 { g.shadow(r.offset(0.0, 10.0), Color(0.0, 0.0, 0.0, 0.45 * focus), h / 2.0, 26.0); }
    g.rect(r, fill, h / 2.0);
    let mut cx = if text.is_empty() { x + (h - 26.0) / 2.0 } else { x + 32.0 };
    if let Some(i) = icon { draw_icon(g, i, cx, y + (h - 26.0) / 2.0, 26.0, ink); cx += iw; }
    if !text.is_empty() { g.text(cx, y + (h - size * 1.2) / 2.0, size, W::Semi, ink, text); }
    r
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Icon { Play, Pause, Restart, Check, Heart, HeartFilled, Search, Subs, Next, Back10, Fwd10 }

/// Simple vector icons drawn from rects/triangles/lines into an s*s box.
pub fn draw_icon(g: &mut Gfx, i: Icon, x: f32, y: f32, s: f32, c: Color) {
    let u = s / 24.0;
    let p = |a: f32, b: f32| (x + a * u, y + b * u);
    match i {
        Icon::Play => g.tri([p(6.0, 3.5), p(6.0, 20.5), p(20.5, 12.0)], c),
        Icon::Pause => { g.rect(Rect::new(x + 5.5 * u, y + 4.0 * u, 4.5 * u, 16.0 * u), c, 1.2 * u); g.rect(Rect::new(x + 14.0 * u, y + 4.0 * u, 4.5 * u, 16.0 * u), c, 1.2 * u); }
        Icon::Restart => {
            for k in 0..14 {
                let a0 = -0.9 + k as f32 * 0.37;
                let a1 = a0 + 0.37;
                if k > 11 { break; }
                g.line(p(12.0 + 8.0 * a0.cos(), 12.0 + 8.0 * a0.sin()), p(12.0 + 8.0 * a1.cos(), 12.0 + 8.0 * a1.sin()), 2.4 * u, c);
            }
            g.tri([p(3.0, 2.5), p(10.5, 4.5), p(4.5, 10.0)], c);
        }
        Icon::Check => { g.line(p(4.5, 12.5), p(9.5, 17.5), 2.8 * u, c); g.line(p(9.5, 17.5), p(19.5, 6.5), 2.8 * u, c); }
        Icon::Heart | Icon::HeartFilled => {
            // two lobes + a point, drawn as a polygon fan around the heart's centre
            let pts: Vec<(f32, f32)> = (0..48).map(|k| {
                let t = k as f32 / 48.0 * std::f32::consts::TAU;
                let hx = 16.0 * t.sin().powi(3);
                let hy = 13.0 * t.cos() - 5.0 * (2.0 * t).cos() - 2.0 * (3.0 * t).cos() - (4.0 * t).cos();
                p(12.0 + hx * 0.58, 11.0 - hy * 0.58)
            }).collect();
            if i == Icon::HeartFilled {
                let c0 = p(12.0, 11.5);
                for k in 0..pts.len() { g.tri([c0, pts[k], pts[(k + 1) % pts.len()]], c); }
            } else {
                for k in 0..pts.len() { g.line(pts[k], pts[(k + 1) % pts.len()], 2.2 * u, c); }
            }
        }
        Icon::Search => { g.ring(Rect::new(x + 3.0 * u, y + 3.0 * u, 14.0 * u, 14.0 * u), c, 7.0 * u, 2.6 * u); g.line(p(15.0, 15.0), p(20.5, 20.5), 2.8 * u, c); }
        Icon::Subs => {
            g.ring(Rect::new(x + 2.0 * u, y + 4.5 * u, 20.0 * u, 15.0 * u), c, 3.0 * u, 2.2 * u);
            g.rect(Rect::new(x + 6.0 * u, y + 13.0 * u, 5.0 * u, 2.2 * u), c, 1.0 * u);
            g.rect(Rect::new(x + 12.5 * u, y + 13.0 * u, 5.5 * u, 2.2 * u), c, 1.0 * u);
            g.rect(Rect::new(x + 6.0 * u, y + 9.0 * u, 8.0 * u, 2.2 * u), c, 1.0 * u);
        }
        Icon::Next => { g.tri([p(4.0, 4.0), p(4.0, 20.0), p(15.0, 12.0)], c); g.rect(Rect::new(x + 16.5 * u, y + 4.0 * u, 3.5 * u, 16.0 * u), c, 1.0 * u); }
        Icon::Back10 | Icon::Fwd10 => {
            let fwd = i == Icon::Fwd10;
            let (a, b) = if fwd { (5.0, 13.0) } else { (19.0, 11.0) };
            g.tri([p(a, 5.0), p(a, 19.0), p(b, 12.0)], c);
            let (a, b) = if fwd { (12.0, 20.0) } else { (12.0, 4.0) };
            g.tri([p(a, 5.0), p(a, 19.0), p(b, 12.0)], c);
        }
    }
}

/// A landscape card (continue watching, episodes): image, progress bar, two caption lines.
#[allow(clippy::too_many_arguments)]
pub fn wide_card(g: &mut Gfx, cx: &mut Cx, c: &Client, it: &Item, r: Rect, f: f32, title: &str, sub: &str) {
    let rr = r.scale_center(1.0 + 0.06 * f);
    let img = Rect::new(rr.x, rr.y, rr.w, rr.h);
    focus_ring(g, img, 16.0, f);
    picture(g, cx, &c.thumb(it, 560), img, 16.0, &initial(title));
    let p = it.progress();
    if p > 0.0 && !it.played() {
        let bar = Rect::new(img.x + 14.0, img.bottom() - 18.0, img.w - 28.0, 6.0);
        g.rect(bar, Color(0.0, 0.0, 0.0, 0.55), 3.0);
        g.rect(Rect::new(bar.x, bar.y, (bar.w * p).max(6.0), bar.h), ACCENT, 3.0);
    }
    if it.played() { played_badge(g, img); }
    let tw = r.w - 6.0;
    let t = g.text.ellipsize(title, 22.0, W::Semi, tw);
    g.text(r.x + 4.0, r.bottom() + 16.0 + 8.0 * f, 22.0, W::Semi, TEXT.a(0.85 + 0.15 * f), &t);
    if !sub.is_empty() {
        let s = g.text.ellipsize(sub, 18.0, W::Regular, tw);
        g.text(r.x + 4.0, r.bottom() + 46.0 + 8.0 * f, 18.0, W::Regular, MUTED, &s);
    }
}

/// A portrait poster card (movies, shows): image + one caption line under it when focused or always.
pub fn poster_card(g: &mut Gfx, cx: &mut Cx, c: &Client, it: &Item, r: Rect, f: f32, caption: bool) {
    let rr = r.scale_center(1.0 + 0.07 * f);
    focus_ring(g, rr, 14.0, f);
    picture(g, cx, &c.poster(it, 360), rr, 14.0, &initial(&it.name));
    if it.played() { played_badge(g, rr); }
    if let Some(n) = it.user_data.as_ref().and_then(|u| u.unplayed_item_count).filter(|n| *n > 0) {
        let s = n.to_string();
        let w = g.text.measure(&s, 17.0, W::Bold, 0.0) + 20.0;
        g.rect(Rect::new(rr.right() - w - 10.0, rr.y + 10.0, w, 30.0), ACCENT, 15.0);
        g.text_c(rr.right() - w / 2.0 - 10.0, rr.y + 15.0, 17.0, W::Bold, Color::hex(0x07080c, 1.0), &s);
    }
    if caption {
        let t = g.text.ellipsize(&it.name, 20.0, W::Semi, r.w);
        g.text(r.x + 2.0, r.bottom() + 14.0 + 10.0 * f, 20.0, W::Semi, TEXT.a(0.62 + 0.38 * f), &t);
        if let Some(y) = it.production_year {
            g.text(r.x + 2.0, r.bottom() + 42.0 + 10.0 * f, 17.0, W::Regular, FAINT, &y.to_string());
        }
    }
}

fn played_badge(g: &mut Gfx, r: Rect) {
    let b = Rect::new(r.right() - 44.0, r.y + 10.0, 34.0, 34.0);
    g.rect(b, Color(0.03, 0.03, 0.05, 0.72), 17.0);
    draw_icon(g, Icon::Check, b.x + 6.0, b.y + 6.0, 22.0, ACCENT);
}

/// Rotating dots spinner.
pub fn spinner(g: &mut Gfx, cx: f32, cy: f32, r: f32, t: f32, c: Color) {
    for k in 0..10 {
        let a = k as f32 / 10.0 * std::f32::consts::TAU + t * 5.0;
        let fade = ((k as f32 / 10.0) + (t * 0.0)).powf(1.6);
        let s = 9.0 * (0.5 + 0.5 * fade);
        g.rect(Rect::new(cx + r * a.cos() - s / 2.0, cy + r * a.sin() - s / 2.0, s, s), c.a(0.15 + 0.85 * fade), s / 2.0);
    }
}

/// The large hero text block shared by home + library (status pill, title or logo, meta, overview).
pub fn hero_text(g: &mut Gfx, cx: &mut Cx, c: &Client, it: &Item, x: f32, bottom: f32, alpha: f32, status: &str) {
    let prev = g.alpha;
    g.alpha *= alpha;
    let meta = meta_line(it);
    let overview = it.overview.clone().unwrap_or_default().replace('\n', " ");
    let ov = g.text.wrap(&overview, 22.0, W::Regular, 680.0, 2);
    let ov_h = ov.len() as f32 * 33.0;
    let mut y = bottom - ov_h;
    for (i, l) in ov.iter().enumerate() { g.text(x, y + i as f32 * 33.0, 22.0, W::Regular, TEXT.a(0.72), l); }
    y -= 46.0;
    g.text(x, y, 20.0, W::Regular, MUTED, &meta);
    let sub = if it.is_episode() { it.subtitle() } else { String::new() };
    if !sub.is_empty() { y -= 44.0; let s = g.text.ellipsize(&sub, 28.0, W::Medium, 820.0); g.text(x, y, 28.0, W::Medium, TEXT, &s); }
    let logo = c.logo(it, 700);
    let mut drew_logo = false;
    if !logo.is_empty() {
        if let Some((tex, a)) = cx.images.get(&logo) {
            let tex = unsafe { &*(tex as *const crate::gfx::Texture) };
            let (mw, mh) = (560.0f32, 170.0f32);
            let s = (mw / tex.w as f32).min(mh / tex.h as f32);
            let (w, h) = (tex.w as f32 * s, tex.h as f32 * s);
            y -= h + 22.0;
            g.image(tex, Rect::new(x, y, w, h), 0.0, a, 0.5, 0.5);
            drew_logo = true;
        }
    }
    if !drew_logo {
        let title = it.title();
        let lines = g.text.wrap(&title, 76.0, W::Bold, 860.0, 2);
        let lh = 80.0;
        y -= lines.len() as f32 * lh + 18.0;
        for (i, l) in lines.iter().enumerate() { g.text_t(x, y + i as f32 * lh, 76.0, W::Bold, TEXT, l, -1.6); }
    }
    if !status.is_empty() { y -= 58.0; pill(g, x, y, status, ACCENT); }
    g.alpha = prev;
}

pub fn meta_line(it: &Item) -> String {
    let mut parts: Vec<String> = vec![];
    if let Some(y) = it.production_year { parts.push(y.to_string()); }
    if let Some(r) = &it.official_rating { parts.push(r.clone()); }
    let m = it.minutes();
    if m > 0 && !it.is_series() { parts.push(if m >= 60 { format!("{} h {} min", m / 60, m % 60) } else { format!("{m} min") }); }
    if it.is_series() { if let Some(n) = it.child_count { parts.push(format!("{n} season{}", if n == 1 { "" } else { "s" })); } }
    if let Some(r) = it.community_rating { parts.push(format!("★ {r:.1}")); }
    if !it.genres.is_empty() { parts.push(it.genres.iter().take(3).cloned().collect::<Vec<_>>().join(", ")); }
    parts.join("  ·  ")
}

/// Status pill text for an item on the hero.
pub fn status_of(it: &Item) -> String {
    if it.progress() > 0.0 && !it.played() { return format!("{} min left", it.minutes_left()); }
    match it.kind.as_str() { "Episode" => "Next up".into(), "Series" => "Series".into(), "Movie" => "Movie".into(), _ => String::new() }
}

/// Backdrop with the launcher's shading (fades into the page on the left and bottom), crossfading.
pub struct Backdrop { cur: String, prev: String, mix: Anim }
impl Backdrop {
    pub fn new() -> Backdrop { Backdrop { cur: String::new(), prev: String::new(), mix: Anim::new(1.0, 5.0) } }
    pub fn set(&mut self, url: String) {
        if url != self.cur { self.prev = std::mem::replace(&mut self.cur, url); self.mix.snap(0.0); self.mix.set(1.0); }
    }
    pub fn update(&mut self, cx: &mut Cx) -> bool {
        // only start the crossfade once the new picture is actually there
        let ready = self.cur.is_empty() || cx.images.get(&self.cur).is_some();
        if ready { self.mix.step(cx.dt) } else { true }
    }
    pub fn draw(&mut self, g: &mut Gfx, cx: &mut Cx, area: Rect, strength: f32) {
        let prev_alpha = g.alpha;
        for (url, a) in [(self.prev.clone(), 1.0 - self.mix.v), (self.cur.clone(), self.mix.v)] {
            if a <= 0.01 || url.is_empty() { continue; }
            if let Some((tex, fade)) = cx.images.get(&url) {
                let tex = unsafe { &*(tex as *const crate::gfx::Texture) };
                g.alpha = prev_alpha * strength;
                g.image(tex, area, 0.0, a * fade, 0.6, 0.2);
            }
        }
        g.alpha = prev_alpha;
        // the four darkening gradients (left side for the text, top for the tabs, bottom into the rows) are one
        // colour, so they are baked into one overlay per area size: one layer for the GPU instead of four
        let key = (area.w as u32, area.h as u32);
        if !g.overlays.iter().any(|(k, _)| *k == key) {
            let (w, h) = ((area.w / 4.0) as usize, (area.h / 4.0) as usize);
            let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t.clamp(0.0, 1.0);
            let mut px = vec![0u8; w * h * 4];
            for y in 0..h {
                for x in 0..w {
                    let (u, py) = ((x as f32 + 0.5) / w as f32, (y as f32 + 0.5) * 4.0);
                    let side = if u < 0.42 { lerp(0.96, 0.82, u / 0.42) } else if u < 0.82 { lerp(0.82, 0.0, (u - 0.42) / 0.4) } else { 0.0 };
                    let top = if py < 200.0 { lerp(0.7, 0.0, py / 200.0) } else { 0.0 };
                    let bottom = if py > area.h * 0.5 { lerp(0.0, 1.0, (py - area.h * 0.5) / (area.h * 0.5)) } else { 0.0 };
                    let a = 1.0 - (1.0 - side) * (1.0 - top) * (1.0 - bottom);
                    let i = (y * w + x) * 4;
                    px[i] = 0x07; px[i + 1] = 0x08; px[i + 2] = 0x0c; px[i + 3] = (a * 255.0 + 0.5) as u8;
                }
            }
            let t = unsafe { g.upload_rgba(w as u32, h as u32, &px) };
            g.overlays.push((key, t));
        }
        let t = &g.overlays.iter().find(|(k, _)| *k == key).unwrap().1 as *const crate::gfx::Texture;
        g.blit(unsafe { &*t }, area);
    }
}
