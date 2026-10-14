//! A poster grid with sorting and an "Unwatched" filter; pages load as you scroll. Shows a library, or every
//! title of a genre / with a person (from a detail page).
use super::*;
use crate::app::PlaySpec;
use crate::jf::Item;
use crate::net::Mailbox;

const COLS: usize = 7;
const PW: f32 = 216.0;
const PH: f32 = 324.0;
const CELL_W: f32 = 245.0;
const CELL_H: f32 = 420.0;
const TOP: f32 = 210.0;
const SORTS: [(&str, &str); 4] = [("A–Z", "SortName"), ("Recently added", "DateCreated"), ("Rating", "CommunityRating"), ("Year", "ProductionYear")];

enum Msg { Page(usize, u64, Result<(Vec<Item>, i64), String>) }

pub struct Library {
    title: String,
    /// what to list: "parentId=…" (a library), "genres=…", "personIds=…"
    query: String,
    unwatched: bool,
    kinds: String,
    mb: Mailbox<Msg>,
    items: Vec<Item>,
    total: i64,
    loading: bool,
    gen: u64,
    /// the focused header chip (sorts, then the filter)
    sort: usize,
    applied_sort: usize,
    idx: usize,
    header: bool,
    sy: Anim,
    focus: Vec<Anim>,
    backdrop: Backdrop,
    appear: Anim,
    error: Option<String>,
}

impl Library {
    pub fn new(view: Item, cx: &mut Cx) -> Library {
        let kinds = match view.collection_type.as_deref() { Some("movies") => "Movie", Some("tvshows") => "Series", Some("boxsets") => "BoxSet", _ => "Movie,Series,Video" }.to_string();
        Self::make(view.name.clone(), format!("parentId={}", view.id), kinds, cx)
    }

    /// Every film and show matching `query` across all libraries (genre or person pages).
    pub fn browse(title: String, query: String, cx: &mut Cx) -> Library { Self::make(title, query, "Movie,Series".into(), cx) }

    fn make(title: String, query: String, kinds: String, cx: &mut Cx) -> Library {
        let mut l = Library { title, query, unwatched: false, kinds, mb: Mailbox::new(), items: vec![], total: -1, loading: false, gen: 0, sort: 0, applied_sort: 0, idx: 0, header: false,
            sy: Anim::new(0.0, 9.0), focus: vec![], backdrop: Backdrop::new(), appear: Anim::new(0.0, 4.0), error: None };
        l.appear.set(1.0);
        l.fetch(cx);
        l
    }

    fn fetch(&mut self, cx: &mut Cx) {
        let Some(c) = cx.client.clone() else { return };
        if self.loading || (self.total >= 0 && self.items.len() as i64 >= self.total) { return; }
        self.loading = true;
        let q = if self.unwatched { format!("{}&filters=IsUnplayed", self.query) } else { self.query.clone() };
        let (tx, start, gen, kinds, sort) = (self.mb.tx.clone(), self.items.len(), self.gen, self.kinds.clone(), SORTS[self.applied_sort].1);
        cx.pool.spawn(move || { let _ = tx.send(Msg::Page(start, gen, c.library(&q, &kinds, sort, start, 140))); crate::net::wake(); });
    }

    fn resort(&mut self, cx: &mut Cx) {
        self.gen += 1; self.items.clear(); self.focus.clear(); self.total = -1; self.idx = 0; self.loading = false; self.sy.snap(0.0);
        self.fetch(cx);
    }
}

impl Screen for Library {
    fn name(&self) -> &'static str { "library" }

    fn key(&mut self, k: Key, cx: &mut Cx) -> Action {
        let n = self.items.len();
        match k {
            Key::Back => return Action::Pop,
            Key::Exit => return Action::Exit,
            Key::Up => { if self.header { } else if self.idx >= COLS { self.idx -= COLS; } else { self.header = true; } }
            Key::Down => { if self.header { self.header = false; } else if self.idx + COLS < n { self.idx += COLS; } else if self.idx / COLS < (n.saturating_sub(1)) / COLS { self.idx = n - 1; } }
            Key::Left => { if self.header { self.sort = self.sort.saturating_sub(1); } else if self.idx % COLS > 0 { self.idx -= 1; } }
            Key::Right => { if self.header { self.sort = (self.sort + 1).min(SORTS.len()); } else if self.idx % COLS < COLS - 1 && self.idx + 1 < n { self.idx += 1; } }
            Key::Ok => {
                if self.header {
                    // the last chip toggles the filter and keeps the sort; the others pick a sort
                    if self.sort == SORTS.len() { self.unwatched = !self.unwatched; self.sort = self.applied_sort; } else { self.applied_sort = self.sort; }
                    self.resort(cx); self.header = false; return Action::None;
                }
                if let Some(it) = self.items.get(self.idx) { return Action::Push(Box::new(super::detail::Detail::new(it.clone(), cx))); }
            }
            Key::Play => { if let Some(it) = self.items.get(self.idx) { if !it.is_folder_like() { return Action::Play(PlaySpec::resume(it.clone())); } } }
            Key::ChDown => { self.idx = (self.idx + COLS * 3).min(n.saturating_sub(1)); }
            Key::ChUp => { self.idx = self.idx.saturating_sub(COLS * 3); }
            _ => {}
        }
        Action::None
    }

    fn hover(&mut self, h: Hot) -> bool {
        if h.row < 0 { let ch = !self.header || self.sort != h.col as usize; self.header = true; self.sort = h.col as usize; return ch; }
        let i = h.col as usize;
        let ch = self.header || self.idx != i;
        self.header = false; self.idx = i; ch
    }

    fn update(&mut self, cx: &mut Cx) -> bool {
        for m in self.mb.drain() {
            let Msg::Page(start, gen, r) = m;
            if gen != self.gen { continue; }
            self.loading = false;
            match r {
                Ok((items, total)) => {
                    if start == self.items.len() {
                        self.total = total;
                        self.focus.extend(items.iter().map(|_| Anim::new(0.0, 14.0)));
                        self.items.extend(items);
                    }
                }
                Err(e) => { self.error = Some(e); self.total = self.items.len() as i64; }
            }
        }
        let row = self.idx / COLS;
        if row + 4 >= self.items.len() / COLS { self.fetch(cx); }
        self.sy.set((row as f32 - 0.6).max(0.0) * CELL_H);
        let mut busy = self.sy.step(cx.dt) | self.appear.step(cx.dt) | self.loading;
        for (i, f) in self.focus.iter_mut().enumerate() { f.set(if !self.header && i == self.idx { 1.0 } else { 0.0 }); busy |= f.step(cx.dt); }
        if let (Some(c), Some(it)) = (cx.client, self.items.get(self.idx)) { self.backdrop.set(c.backdrop(it, 1920)); }
        busy | self.backdrop.update(cx)
    }

    fn draw(&mut self, g: &mut Gfx, cx: &mut Cx) {
        let Some(c) = cx.client.clone() else { return };
        background(g, cx.t);
        self.backdrop.draw(g, cx, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.32);
        g.alpha = smooth(self.appear.v);
        // grid
        g.push_clip(Rect::new(0.0, 150.0, 1920.0, 930.0));
        let first_row = ((self.sy.v - 60.0) / CELL_H).floor().max(0.0) as usize;
        for i in (first_row * COLS)..self.items.len() {
            let (r, col) = (i / COLS, i % COLS);
            let y = TOP + r as f32 * CELL_H - self.sy.v;
            if y > 1100.0 { break; }
            let rect = Rect::new(PAD + col as f32 * CELL_W, y, PW, PH);
            let it = self.items[i].clone();
            poster_card(g, cx, &c, &it, rect, self.focus.get(i).map_or(0.0, |f| f.v), true);
            cx.hot.push((rect, Hot { row: 0, col: i as i32 }));
        }
        g.pop_clip();
        // header (drawn over a fade so posters slide under it)
        g.vgrad(Rect::new(0.0, 0.0, 1920.0, 230.0), Color::hex(0x07080c, 0.96), Color::hex(0x07080c, 0.0), 0.0);
        let title = g.text.ellipsize(&self.title, 56.0, W::Bold, 820.0);
        g.text_t(PAD, 52.0, 56.0, W::Bold, TEXT, &title, -1.0);
        let count = if self.total >= 0 { format!("{} {}", self.total, if self.unwatched { "unwatched" } else { "titles" }) } else { String::new() };
        let nw = g.text.measure(&title, 56.0, W::Bold, -1.0);
        g.text(PAD + nw + 24.0, 76.0, 22.0, W::Regular, MUTED, &count);
        let mut x = 1920.0 - PAD;
        let mut rects = vec![];
        let chips: Vec<(usize, &str)> = SORTS.iter().map(|s| s.0).chain(std::iter::once("Unwatched")).enumerate().collect();
        for &(i, name) in chips.iter().rev() {
            let w = g.text.measure(name, 21.0, W::Semi, 0.0) + 40.0;
            x -= w;
            rects.push((i, Rect::new(x, 64.0, w, 46.0), name));
            x -= if i == SORTS.len() { 30.0 } else { 10.0 };
        }
        for (i, r, name) in rects {
            let sel = if i == SORTS.len() { self.unwatched } else { i == self.applied_sort };
            let f = if self.header && i == self.sort { 1.0 } else { 0.0 };
            g.rect(r, if f > 0.0 { Color(1.0, 1.0, 1.0, 1.0) } else if sel { SURFACE_HI } else { SURFACE }, 23.0);
            g.text(r.x + 20.0, r.y + 11.0, 21.0, W::Semi, if f > 0.0 { BG } else if sel { TEXT } else { MUTED }, name);
            cx.hot.push((r, Hot { row: -1, col: i as i32 }));
        }
        if self.items.is_empty() && self.loading { spinner(g, 960.0, 540.0, 22.0, cx.t, MUTED); }
        if self.items.is_empty() && !self.loading && self.total == 0 {
            g.text_c(960.0, 500.0, 28.0, W::Semi, MUTED, if self.unwatched { "You've seen everything here" } else { "Nothing here" });
        }
        if let Some(e) = &self.error { g.text(PAD, 1020.0, 20.0, W::Regular, Color::hex(0xff9b9b, 1.0), e); }
        g.alpha = 1.0;
    }
}
