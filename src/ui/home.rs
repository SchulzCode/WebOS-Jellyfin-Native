//! Home: the focused item's backdrop + text as a hero, a top tab bar, and rows below it
//! (continue watching, next up, recently added per library, libraries).
use super::*;
use crate::app::PlaySpec;
use crate::jf::Item;
use crate::net::Mailbox;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq)]
enum Kind { Wide, Poster, Library }

struct Row { key: String, title: String, kind: Kind, items: Vec<Item>, col: usize, sx: Anim, focus: Vec<Anim> }

enum Msg { Row(usize, Row), Done(Option<String>) }

#[derive(Clone)]
enum Tab { Home, Lib(Item), Search, Settings }

pub struct Home {
    mb: Mailbox<Msg>,
    rows: Vec<(usize, Row)>,
    tabs: Vec<(String, Tab)>,
    tab: usize,
    row: i32, // -1 = tab bar
    sy: Anim,
    tabs_focus: Anim,
    backdrop: Backdrop,
    hero: Option<Item>,
    hero_a: Anim,
    hero_pending: Option<(Item, Instant)>,
    loading: bool,
    error: Option<String>,
    loaded_at: Option<Instant>,
    appear: Anim,
}

const WIDE: (f32, f32) = (352.0, 198.0);
const POSTER: (f32, f32) = (196.0, 294.0);
const GAP: f32 = 26.0;
const ROWS_TOP: f32 = 640.0;

impl Home {
    pub fn new(cx: &mut Cx) -> Home {
        let mut h = Home {
            mb: Mailbox::new(), rows: vec![], tabs: vec![("Home".into(), Tab::Home), ("Search".into(), Tab::Search), ("Settings".into(), Tab::Settings)],
            tab: 0, row: 0, sy: Anim::new(0.0, 9.0), tabs_focus: Anim::new(0.0, 12.0), backdrop: Backdrop::new(), hero: None,
            hero_a: Anim::new(0.0, 7.0), hero_pending: None, loading: true, error: None, loaded_at: None, appear: Anim::new(0.0, 3.5),
        };
        h.appear.set(1.0);
        h.load(cx);
        h
    }

    fn load(&mut self, cx: &mut Cx) {
        let Some(c) = cx.client.clone() else { return };
        self.loading = true;
        self.loaded_at = Some(Instant::now());
        let tx = self.mb.tx.clone();
        let mk = |key: &str, title: &str, kind: Kind, items: Vec<Item>| Row { key: key.into(), title: title.into(), kind, focus: items.iter().map(|_| Anim::new(0.0, 14.0)).collect(), items, col: 0, sx: Anim::new(0.0, 10.0) };
        let (c1, t1) = (c.clone(), tx.clone());
        cx.pool.spawn(move || {
            match c1.resume() {
                Ok(v) => { let _ = t1.send(Msg::Row(0, mk("resume", "Continue watching", Kind::Wide, v))); }
                Err(e) => { let _ = t1.send(Msg::Done(Some(e))); }
            }
            crate::net::wake();
        });
        let (c2, t2) = (c.clone(), tx.clone());
        cx.pool.spawn(move || {
            if let Ok(v) = c2.next_up() { let _ = t2.send(Msg::Row(1, mk("next", "Next up", Kind::Wide, v))); }
            crate::net::wake();
        });
        let (c3, t3) = (c, tx);
        cx.pool.spawn(move || {
            match c3.views() {
                Ok(views) => {
                    let views: Vec<Item> = views.into_iter().filter(|v| matches!(v.collection_type.as_deref(), Some("movies") | Some("tvshows") | Some("homevideos") | Some("mixed") | None)).collect();
                    let _ = t3.send(Msg::Row(100, mk("libs", "Your libraries", Kind::Library, views.clone())));
                    crate::net::wake();
                    for (i, v) in views.iter().enumerate() {
                        if let Ok(items) = c3.latest(&v.id) {
                            let _ = t3.send(Msg::Row(10 + i, mk(&format!("latest-{}", v.id), &format!("Recently added in {}", v.name), Kind::Poster, items)));
                            crate::net::wake();
                        }
                    }
                    let _ = t3.send(Msg::Done(None));
                }
                Err(e) => { let _ = t3.send(Msg::Done(Some(e))); }
            }
            crate::net::wake();
        });
    }

    fn visible_rows(&self) -> Vec<usize> { (0..self.rows.len()).filter(|&i| !self.rows[i].1.items.is_empty()).collect() }

    fn cur(&self) -> Option<&Row> { let v = self.visible_rows(); v.get(self.row.max(0) as usize).map(|&i| &self.rows[i].1) }

    fn focused_item(&self) -> Option<Item> {
        let r = self.cur()?;
        if r.kind == Kind::Library { return None; }
        r.items.get(r.col).cloned()
    }

    fn row_h(k: Kind) -> f32 { match k { Kind::Wide | Kind::Library => 58.0 + WIDE.1 + 104.0, Kind::Poster => 58.0 + POSTER.1 + 96.0 } }

    fn open(&self, it: &Item, cx: &mut Cx) -> Action {
        if it.kind == "CollectionFolder" || it.collection_type.is_some() {
            return Action::Push(Box::new(super::library::Library::new(it.clone(), cx)));
        }
        Action::Push(Box::new(super::detail::Detail::new(it.clone(), cx)))
    }
}

impl Screen for Home {
    fn name(&self) -> &'static str { "home" }

    fn resumed(&mut self, cx: &mut Cx) {
        // back from a detail page or a film: progress changed, refresh (cheap, a few small requests)
        if self.loaded_at.map_or(true, |t| t.elapsed() > Duration::from_secs(3)) { self.load(cx); }
    }

    fn key(&mut self, k: Key, cx: &mut Cx) -> Action {
        if self.error.is_some() && self.visible_rows().is_empty() && k == Key::Ok && !self.loading { self.error = None; self.load(cx); return Action::None; }
        let vis = self.visible_rows();
        match k {
            Key::Up => {
                if self.row > 0 { self.row -= 1; } else if self.row == 0 { self.row = -1; }
            }
            Key::Down => {
                if self.row < 0 { if !vis.is_empty() { self.row = 0; } }
                else if (self.row as usize) + 1 < vis.len() { self.row += 1; }
            }
            Key::Left | Key::Right => {
                let d: i32 = if k == Key::Left { -1 } else { 1 };
                if self.row < 0 {
                    self.tab = (self.tab as i32 + d).clamp(0, self.tabs.len() as i32 - 1) as usize;
                } else if let Some(&ri) = vis.get(self.row as usize) {
                    let r = &mut self.rows[ri].1;
                    r.col = (r.col as i32 + d).clamp(0, r.items.len() as i32 - 1) as usize;
                }
            }
            Key::Ok => {
                if self.row < 0 {
                    return match self.tabs[self.tab].1.clone() {
                        Tab::Home => { self.row = 0; Action::None }
                        Tab::Lib(v) => Action::Push(Box::new(super::library::Library::new(v, cx))),
                        Tab::Search => Action::Push(Box::new(super::search::Search::new(cx))),
                        Tab::Settings => Action::Push(Box::new(super::settings::Settings::new(cx))),
                    };
                }
                if let Some(r) = self.cur() { if let Some(it) = r.items.get(r.col).cloned() { return self.open(&it, cx); } }
            }
            Key::Play => {
                if let Some(it) = self.focused_item() {
                    if !it.is_folder_like() { return Action::Play(PlaySpec::resume(it)); }
                }
            }
            Key::Back => {
                if self.row != 0 || self.cur().map_or(false, |r| r.col != 0) {
                    if self.row < 0 { self.row = 0; }
                    else { if let Some(&ri) = vis.get(self.row as usize) { self.rows[ri].1.col = 0; } self.row = 0; }
                } else { return Action::Exit; }
            }
            Key::Exit => return Action::Exit,
            _ => {}
        }
        Action::None
    }

    fn hover(&mut self, h: Hot) -> bool {
        let vis = self.visible_rows();
        if h.row < 0 {
            let ch = self.row != -1 || self.tab != h.col as usize;
            self.row = -1; self.tab = h.col as usize; return ch;
        }
        let Some(&ri) = vis.get(h.row as usize) else { return false };
        let ch = self.row != h.row || self.rows[ri].1.col != h.col as usize;
        self.row = h.row;
        self.rows[ri].1.col = h.col as usize;
        ch
    }

    fn update(&mut self, cx: &mut Cx) -> bool {
        if self.error.as_deref().map_or(false, crate::jf::retryable) && !self.loading && self.loaded_at.map_or(true, |t| t.elapsed() > Duration::from_secs(15)) {
            crate::log!("home: retrying after {:?}", self.error);
            self.load(cx);
        }
        let mut busy = false;
        for m in self.mb.drain() {
            match m {
                Msg::Row(order, mut row) => {
                    if row.kind == Kind::Library {
                        // libraries also become tabs (Home · Movies · Shows · Search · Settings)
                        let mut tabs = vec![("Home".to_string(), Tab::Home)];
                        for v in &row.items { tabs.push((v.name.clone(), Tab::Lib(v.clone()))); }
                        tabs.push(("Search".into(), Tab::Search));
                        tabs.push(("Settings".into(), Tab::Settings));
                        self.tabs = tabs;
                        self.tab = self.tab.min(self.tabs.len() - 1);
                    }
                    if let Some(pos) = self.rows.iter().position(|(_, r)| r.key == row.key) {
                        // refresh in place: keep the focused item focused if it is still there
                        let old = &self.rows[pos].1;
                        let keep = old.items.get(old.col).map(|i| i.id.clone());
                        row.col = keep.and_then(|id| row.items.iter().position(|i| i.id == id)).unwrap_or(old.col.min(row.items.len().saturating_sub(1)));
                        row.sx = old.sx;
                        self.rows[pos] = (order, row);
                    } else {
                        self.rows.push((order, row));
                        self.rows.sort_by_key(|(o, _)| *o);
                    }
                }
                Msg::Done(e) => { self.loading = false; self.error = e; }
            }
        }
        let vis = self.visible_rows();
        if !vis.is_empty() && self.row >= vis.len() as i32 { self.row = vis.len() as i32 - 1; }
        // vertical scroll: the focused row sits at ROWS_TOP
        let mut y = 0.0;
        for (n, &ri) in vis.iter().enumerate() { if n as i32 >= self.row.max(0) { break; } y += Self::row_h(self.rows[ri].1.kind); }
        self.sy.set(y);
        busy |= self.sy.step(cx.dt);
        self.tabs_focus.set(if self.row < 0 { 1.0 } else { 0.0 });
        busy |= self.tabs_focus.step(cx.dt);
        for (n, &ri) in vis.iter().enumerate() {
            let focused_row = n as i32 == self.row;
            let r = &mut self.rows[ri].1;
            let (w, _) = if r.kind == Kind::Poster { POSTER } else { WIDE };
            let max_off = ((r.items.len() as f32) * (w + GAP) - (1920.0 - PAD - 60.0)).max(0.0);
            r.sx.set(((r.col as f32 - 1.0).max(0.0) * (w + GAP)).min(max_off));
            busy |= r.sx.step(cx.dt);
            for (i, f) in r.focus.iter_mut().enumerate() { f.set(if focused_row && i == r.col { 1.0 } else { 0.0 }); busy |= f.step(cx.dt); }
        }
        // hero follows focus, with a short delay so fast scrolling doesn't thrash backdrops
        let want = self.focused_item();
        if want.as_ref().map(|i| &i.id) != self.hero.as_ref().map(|i| &i.id) {
            match &self.hero_pending {
                Some((p, at)) if want.as_ref().map(|i| &i.id) == Some(&p.id) => {
                    if at.elapsed() > Duration::from_millis(140) {
                        let it = want.clone().unwrap();
                        if let Some(c) = cx.client { self.backdrop.set(c.backdrop(&it, 1920)); }
                        self.hero = Some(it);
                        self.hero_a.snap(0.0); self.hero_a.set(1.0);
                        self.hero_pending = None;
                    }
                    busy = true;
                }
                _ => {
                    if let Some(w) = want { self.hero_pending = Some((w, Instant::now())); busy = true; }
                }
            }
        }
        busy |= self.hero_a.step(cx.dt);
        busy |= self.backdrop.update(cx);
        busy |= self.appear.step(cx.dt);
        busy |= self.loading;
        // warm the neighbours' backdrops
        if let (Some(c), Some(r)) = (cx.client, self.cur()) {
            for d in [1usize, 2] { if let Some(it) = r.items.get(r.col + d) { cx.images.prefetch(&c.backdrop(it, 1920)); } }
        }
        busy
    }

    fn draw(&mut self, g: &mut Gfx, cx: &mut Cx) {
        let Some(c) = cx.client.clone() else { return };
        background(g, cx.t);
        self.backdrop.draw(g, cx, Rect::new(0.0, 0.0, 1920.0, 820.0), 1.0);
        let a = smooth(self.appear.v);
        g.alpha = a;

        // top bar: tabs left, clock right
        let mut x = PAD;
        let tf = self.tabs_focus.v;
        for (i, (name, _)) in self.tabs.iter().enumerate() {
            let sel = i == self.tab;
            let size = 24.0;
            let w = g.text.measure(name, size, W::Semi, 0.0) + 40.0;
            let r = Rect::new(x - 20.0, 40.0, w, 52.0);
            let f = if sel { tf } else { 0.0 };
            if f > 0.01 { g.rect(r, Color(1.0, 1.0, 1.0, f), 26.0); }
            let ink = if sel && tf < 0.5 { TEXT } else if sel { TEXT.lerp(BG, f) } else { MUTED };
            g.text(x, 52.0, size, W::Semi, ink, name);
            if sel && i == 0 && tf < 0.5 { g.rect(Rect::new(x, 92.0, w - 40.0, 3.0), ACCENT.a(1.0 - tf * 2.0), 1.5); }
            cx.hot.push((r, Hot { row: -1, col: i as i32 }));
            x += w + 8.0;
        }
        g.text_r(1920.0 - PAD, 44.0, 44.0, W::Medium, TEXT, &hhmm(0));

        // hero
        if let Some(it) = self.hero.clone() {
            let ha = smooth(self.hero_a.v);
            let fade_up = (self.sy.v / 200.0).min(1.0) * 0.0;
            hero_text(g, cx, &c, &it, PAD, ROWS_TOP - 74.0 - (1.0 - ha) * 12.0, ha * (1.0 - fade_up), &status_of(&it));
        } else if !self.loading && self.error.is_none() && self.visible_rows().iter().all(|&i| self.rows[i].1.kind == Kind::Library) {
            g.text_t(PAD, 380.0, 64.0, W::Bold, TEXT, "Nothing to continue", -1.2);
            g.text(PAD, 470.0, 24.0, W::Regular, MUTED, "Pick something from your libraries below.");
        }
        if let Some(e) = &self.error {
            if self.visible_rows().is_empty() {
                // nothing loaded at all: say it plainly, offer a retry (it also retries by itself)
                g.text_t(PAD, 360.0, 60.0, W::Bold, TEXT, "Can't reach Jellyfin", -1.2);
                let msg: String = e.chars().take(1).flat_map(|c| c.to_uppercase()).chain(e.chars().skip(1)).collect();
                for (i, l) in g.text.wrap(&msg, 26.0, W::Regular, 1100.0, 3).iter().enumerate() { g.text(PAD, 450.0 + i as f32 * 38.0, 26.0, W::Regular, MUTED, l); }
                g.text(PAD, 580.0, 22.0, W::Medium, FAINT, if self.loading { "Trying again…" } else { "OK  Try again   ·   it also retries by itself" });
            } else {
                g.text(PAD, 560.0, 22.0, W::Regular, Color::hex(0xff9b9b, 1.0), &format!("Couldn't load everything: {e}"));
            }
        }

        // rows
        let vis = self.visible_rows();
        let mut y = ROWS_TOP - self.sy.v;
        g.push_clip(Rect::new(0.0, 0.0, 1920.0, 1080.0));
        for (n, &ri) in vis.iter().enumerate() {
            let (kind, h) = { let r = &self.rows[ri].1; (r.kind, Self::row_h(r.kind)) };
            if y > 1080.0 { break; }
            // rows that scroll up into the hero fade out
            let fa = ((y - (ROWS_TOP - 120.0)) / 120.0).clamp(0.0, 1.0);
            if y + h > 0.0 && fa > 0.0 {
                let prev = g.alpha;
                g.alpha = a * fa;
                let r = &self.rows[ri].1;
                label(g, PAD, y, &r.title);
                let (w, ch) = if kind == Kind::Poster { POSTER } else { WIDE };
                let top = y + 52.0;
                let items = r.items.clone();
                let (sx, col) = (r.sx.v, r.col);
                let focus: Vec<f32> = r.focus.iter().map(|f| f.v).collect();
                let first = ((sx / (w + GAP)).floor() as usize).saturating_sub(1);
                for (i, it) in items.iter().enumerate().skip(first) {
                    let cxp = PAD + i as f32 * (w + GAP) - sx;
                    if cxp > 1920.0 { break; }
                    let rect = Rect::new(cxp, top, w, ch);
                    let f = focus.get(i).copied().unwrap_or(0.0);
                    match kind {
                        Kind::Wide => {
                            let (t, s) = if it.is_episode() { (it.title(), it.subtitle()) } else { (it.name.clone(), it.subtitle()) };
                            wide_card(g, cx, &c, it, rect, f, &t, &s);
                        }
                        Kind::Poster => poster_card(g, cx, &c, it, rect, f, true),
                        Kind::Library => {
                            let rr = rect.scale_center(1.0 + 0.05 * f);
                            focus_ring(g, rr, 16.0, f);
                            let url = c.img(&it.id, "Primary", it.image_tags.get("Primary").map(|s| s.as_str()), 560);
                            picture(g, cx, &url, rr, 16.0, "");
                            g.vgrad(Rect::new(rr.x, rr.y + rr.h * 0.45, rr.w, rr.h * 0.55), Color(0.0, 0.0, 0.0, 0.0), Color(0.0, 0.0, 0.0, 0.7), 16.0);
                            g.text(rr.x + 22.0, rr.bottom() - 52.0, 28.0, W::Bold, TEXT, &it.name);
                        }
                    }
                    cx.hot.push((rect, Hot { row: n as i32, col: i as i32 }));
                    let _ = col;
                }
                g.alpha = prev;
            }
            y += h;
        }
        g.pop_clip();
        if self.loading && self.rows.is_empty() { spinner(g, 960.0, 760.0, 22.0, cx.t, MUTED); }
        g.alpha = 1.0;
    }
}
