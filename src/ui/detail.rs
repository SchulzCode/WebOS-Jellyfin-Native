//! Item page: backdrop, logo/title, metadata, actions; seasons + episodes for shows, similar titles for films;
//! genres + cast at the bottom (each opens a grid of matching titles).
use super::*;
use crate::app::PlaySpec;
use crate::jf::{enc, Item, Person};
use crate::net::Mailbox;

enum Msg { Item(Result<Item, String>), Seasons(Vec<Item>), Episodes(String, Vec<Item>), Similar(Vec<Item>), Toggled }

#[derive(Clone, Copy, PartialEq, Debug)]
enum Zone { Buttons, Seasons, Row, People }

#[derive(Clone, Copy, PartialEq)]
enum Btn { Play, Restart, Version, Watched, Favorite }

/// The bottom row: genres, then the cast.
#[derive(Clone)]
enum Chip { Genre(String), Person(Person) }

pub struct Detail {
    item: Item,
    full: bool,
    mb: Mailbox<Msg>,
    seasons: Vec<Item>,
    season: usize,
    /// the season chip under the pointer (a click picks it; the remote's Left/Right pick at once)
    scol: usize,
    episodes: Vec<Item>,
    episodes_of: String,
    similar: Vec<Item>,
    zone: Zone,
    btn: usize,
    col: usize,
    sx: Anim,
    focus: Vec<Anim>,
    bfocus: Vec<Anim>,
    backdrop: Backdrop,
    appear: Anim,
    shown: Option<Item>,
    shown_a: Anim,
    /// chosen version (index into media_sources) when there are several
    version: usize,
    pcol: usize,
    px: Anim,
    /// the page slides up while the genres/cast row is focused
    sy: Anim,
}

const PW: f32 = 170.0;
const PGAP: f32 = 22.0;

const EW: f32 = 340.0;
const EH: f32 = 191.0;
const GAP: f32 = 26.0;

impl Detail {
    pub fn new(item: Item, cx: &mut Cx) -> Detail {
        let mut d = Detail {
            item, full: false, mb: Mailbox::new(), seasons: vec![], season: 0, scol: 0, episodes: vec![], episodes_of: String::new(), similar: vec![],
            zone: Zone::Buttons, btn: 0, col: 0, sx: Anim::new(0.0, 10.0), focus: vec![], bfocus: (0..5).map(|_| Anim::new(0.0, 14.0)).collect(),
            backdrop: Backdrop::new(), appear: Anim::new(0.0, 4.0), shown: None, shown_a: Anim::new(1.0, 7.0),
            version: 0, pcol: 0, px: Anim::new(0.0, 10.0), sy: Anim::new(0.0, 8.0),
        };
        d.appear.set(1.0);
        if let Some(c) = cx.client { d.backdrop.set(c.backdrop(&d.item, 1920)); }
        d.reload(cx);
        d
    }

    fn reload(&mut self, cx: &mut Cx) {
        let Some(c) = cx.client.clone() else { return };
        let (tx, id) = (self.mb.tx.clone(), self.item.id.clone());
        cx.pool.spawn(move || { let _ = tx.send(Msg::Item(c.item(&id))); crate::net::wake(); });
    }

    fn series_id(&self) -> Option<String> {
        if self.item.is_series() { Some(self.item.id.clone()) } else if self.item.is_episode() { self.item.series_id.clone() } else { None }
    }

    fn load_episodes(&mut self, cx: &mut Cx) {
        let (Some(c), Some(sid), Some(season)) = (cx.client.clone(), self.series_id(), self.seasons.get(self.season).map(|s| s.id.clone())) else { return };
        if self.episodes_of == season { return; }
        self.episodes_of = season.clone();
        let tx = self.mb.tx.clone();
        cx.pool.spawn(move || { let v = c.episodes(&sid, &season).unwrap_or_default(); let _ = tx.send(Msg::Episodes(season, v)); crate::net::wake(); });
    }

    fn row_items(&self) -> &Vec<Item> { if self.series_id().is_some() { &self.episodes } else { &self.similar } }

    fn chips(&self) -> Vec<Chip> {
        let mut v: Vec<Chip> = self.item.genres.iter().take(4).cloned().map(Chip::Genre).collect();
        let mut seen = std::collections::HashSet::new();
        v.extend(self.item.people.iter().filter(|p| matches!(p.kind.as_str(), "Actor" | "Director" | "GuestStar")).filter(|p| seen.insert(p.id.clone())).take(16).cloned().map(Chip::Person));
        v
    }

    fn source_id(&self) -> Option<String> {
        (self.item.media_sources.len() > 1).then(|| self.item.media_sources.get(self.version).map(|m| m.id.clone())).flatten()
    }

    fn version_label(&self) -> String {
        let Some(ms) = self.item.media_sources.get(self.version) else { return String::new() };
        let v = ms.media_streams.iter().find(|s| s.kind == "Video");
        let res = v.and_then(|v| v.height).map(|h| if h >= 1800 { "4K".to_string() } else { format!("{h}p") });
        ms.name.clone().filter(|n| !n.is_empty()).or(res).unwrap_or_else(|| format!("Version {}", self.version + 1))
    }

    fn buttons(&self) -> Vec<(Btn, String, Icon)> {
        let it = &self.item;
        let mut v = vec![];
        if it.is_series() {
            let next = self.next_episode();
            let label = match &next { Some(e) if e.progress() > 0.0 => format!("Resume {}", e.episode_code()), Some(e) => format!("Play {}", e.episode_code()), None => "Play".into() };
            v.push((Btn::Play, label, Icon::Play));
        } else if it.progress() > 0.0 && !it.played() {
            v.push((Btn::Play, format!("Resume · {} min left", it.minutes_left()), Icon::Play));
            v.push((Btn::Restart, "From the start".into(), Icon::Restart));
        } else {
            v.push((Btn::Play, "Play".into(), Icon::Play));
        }
        if it.media_sources.len() > 1 { v.push((Btn::Version, self.version_label(), Icon::Subs)); }
        v.push((Btn::Watched, if it.played() { "Watched".into() } else { "Mark watched".into() }, Icon::Check));
        v.push((Btn::Favorite, String::new(), if it.favorite() { Icon::HeartFilled } else { Icon::Heart }));
        v
    }

    /// For a show: the first episode in the loaded season that isn't finished (else the first).
    fn next_episode(&self) -> Option<Item> {
        self.episodes.iter().find(|e| !e.played()).or(self.episodes.first()).cloned()
    }

    fn queue_after(&self, ep: &Item) -> Vec<Item> {
        let i = self.episodes.iter().position(|e| e.id == ep.id).map(|i| i + 1).unwrap_or(self.episodes.len());
        self.episodes[i..].to_vec()
    }

    fn act(&mut self, b: Btn, cx: &mut Cx) -> Action {
        let it = self.item.clone();
        match b {
            Btn::Play => {
                if it.is_series() { return match self.next_episode() { Some(e) => { let q = self.queue_after(&e); Action::Play(PlaySpec::resume(e).with_queue(q)) } None => Action::Toast("No episodes yet".into()) }; }
                let q = if it.is_episode() { self.queue_after(&it) } else { vec![] };
                let src = self.source_id();
                Action::Play(PlaySpec::resume(it).with_queue(q).with_source(src))
            }
            Btn::Restart => { let q = if it.is_episode() { self.queue_after(&it) } else { vec![] }; let src = self.source_id(); Action::Play(PlaySpec::from_start(it).with_queue(q).with_source(src)) }
            Btn::Version => { self.version = (self.version + 1) % it.media_sources.len().max(1); Action::None }
            Btn::Watched | Btn::Favorite => {
                let Some(c) = cx.client.clone() else { return Action::None };
                let tx = self.mb.tx.clone();
                let id = it.id.clone();
                let ud = self.item.user_data.get_or_insert_with(Default::default);
                let (watched, fav) = (b == Btn::Watched, b == Btn::Favorite);
                if watched { ud.played = !ud.played; if ud.played { ud.played_percentage = None; ud.playback_position_ticks = 0; } }
                if fav { ud.is_favorite = !ud.is_favorite; }
                let (p, f) = (ud.played, ud.is_favorite);
                cx.pool.spawn(move || {
                    let r = if watched { c.set_played(&id, p) } else { c.set_favorite(&id, f) };
                    if let Err(e) = r { crate::log!("toggle: {e}"); }
                    let _ = tx.send(Msg::Toggled); crate::net::wake();
                });
                Action::None
            }
        }
    }
}

impl Screen for Detail {
    fn name(&self) -> &'static str { "detail" }

    fn resumed(&mut self, cx: &mut Cx) { self.episodes_of.clear(); self.reload(cx); }

    fn key(&mut self, k: Key, cx: &mut Cx) -> Action {
        let has_seasons = self.seasons.len() > 1;
        let has_row = !self.row_items().is_empty();
        let has_people = !self.chips().is_empty();
        let nb = self.buttons().len();
        match k {
            Key::Back => return Action::Pop,
            Key::Exit => return Action::Exit,
            Key::Up => {
                self.zone = match self.zone {
                    Zone::People => if has_row { Zone::Row } else { Zone::Buttons },
                    Zone::Row => if has_seasons { Zone::Seasons } else { Zone::Buttons },
                    _ => Zone::Buttons,
                };
                self.scol = self.season;
            }
            Key::Down => {
                self.zone = match self.zone {
                    Zone::Buttons => if has_seasons { Zone::Seasons } else if has_row { Zone::Row } else if has_people { Zone::People } else { Zone::Buttons },
                    Zone::Seasons => if has_row { Zone::Row } else { Zone::Seasons },
                    Zone::Row => if has_people { Zone::People } else { Zone::Row },
                    Zone::People => Zone::People,
                };
                self.scol = self.season;
            }
            Key::Left | Key::Right => {
                let d: i32 = if k == Key::Left { -1 } else { 1 };
                match self.zone {
                    Zone::Buttons => self.btn = (self.btn as i32 + d).clamp(0, nb as i32 - 1) as usize,
                    Zone::Seasons => {
                        let s = (self.season as i32 + d).clamp(0, self.seasons.len() as i32 - 1) as usize;
                        if s != self.season { self.season = s; self.col = 0; self.load_episodes(cx); }
                        self.scol = s;
                    }
                    Zone::Row => self.col = (self.col as i32 + d).clamp(0, self.row_items().len() as i32 - 1) as usize,
                    Zone::People => self.pcol = (self.pcol as i32 + d).clamp(0, self.chips().len() as i32 - 1) as usize,
                }
            }
            Key::Ok | Key::Play => match self.zone {
                Zone::Buttons => {
                    let b = if k == Key::Ok { self.buttons()[self.btn.min(nb - 1)].0 } else { Btn::Play };
                    return self.act(b, cx);
                }
                Zone::Seasons => {
                    if self.scol != self.season && self.scol < self.seasons.len() { self.season = self.scol; self.col = 0; self.load_episodes(cx); }
                    else { self.zone = Zone::Row; }
                }
                Zone::People => {
                    return match self.chips().get(self.pcol).cloned() {
                        Some(Chip::Genre(g)) => Action::Push(Box::new(super::library::Library::browse(g.clone(), format!("genres={}", enc(&g)), cx))),
                        Some(Chip::Person(p)) => Action::Push(Box::new(super::library::Library::browse(p.name.clone(), format!("personIds={}", p.id), cx))),
                        None => Action::None,
                    };
                }
                Zone::Row => {
                    if let Some(it) = self.row_items().get(self.col).cloned() {
                        if self.series_id().is_some() {
                            let q = self.queue_after(&it);
                            return Action::Play(PlaySpec::resume(it).with_queue(q));
                        }
                        if k == Key::Ok { return Action::Push(Box::new(Detail::new(it, cx))); }
                        return Action::Play(PlaySpec::resume(it));
                    }
                }
            },
            _ => {}
        }
        Action::None
    }

    fn hover(&mut self, h: Hot) -> bool {
        let (z, c) = match h.row { 0 => (Zone::Buttons, h.col as usize), 1 => (Zone::Seasons, h.col as usize), 3 => (Zone::People, h.col as usize), _ => (Zone::Row, h.col as usize) };
        let changed = self.zone != z || match z { Zone::Buttons => self.btn != c, Zone::Seasons => self.scol != c, Zone::Row => self.col != c, Zone::People => self.pcol != c };
        self.zone = z;
        match z { Zone::Buttons => self.btn = c, Zone::Seasons => self.scol = c, Zone::Row => self.col = c, Zone::People => self.pcol = c }
        changed
    }

    fn update(&mut self, cx: &mut Cx) -> bool {
        for m in self.mb.drain() {
            match m {
                Msg::Item(Ok(it)) => {
                    let first = !self.full;
                    self.item = it;
                    self.full = true;
                    if let Some(c) = cx.client.clone() {
                        self.backdrop.set(c.backdrop(&self.item, 1920));
                        if first {
                            let tx = self.mb.tx.clone();
                            if let Some(sid) = self.series_id() {
                                let c2 = c.clone();
                                cx.pool.spawn(move || { let v = c2.seasons(&sid).unwrap_or_default(); let _ = tx.send(Msg::Seasons(v)); crate::net::wake(); });
                            } else {
                                let id = self.item.id.clone();
                                cx.pool.spawn(move || { let v = c.similar(&id).unwrap_or_default(); let _ = tx.send(Msg::Similar(v)); crate::net::wake(); });
                            }
                        } else if self.series_id().is_some() { self.episodes_of.clear(); self.load_episodes(cx); }
                    }
                }
                Msg::Item(Err(e)) => crate::log!("detail: {e}"),
                Msg::Seasons(v) => {
                    // start on the season of this episode, or the first season with something unwatched
                    self.season = if self.item.is_episode() { v.iter().position(|s| Some(&s.id) == self.item.season_id.as_ref()).unwrap_or(0) }
                        else { v.iter().position(|s| s.user_data.as_ref().and_then(|u| u.unplayed_item_count).unwrap_or(0) > 0).unwrap_or(0) };
                    self.scol = self.season;
                    self.seasons = v;
                    self.load_episodes(cx);
                }
                Msg::Episodes(sid, v) => {
                    if sid == self.episodes_of {
                        if self.item.is_episode() { if let Some(i) = v.iter().position(|e| e.id == self.item.id) { self.col = i; } }
                        else if self.col == 0 { self.col = v.iter().position(|e| !e.played()).unwrap_or(0); }
                        self.episodes = v;
                        self.focus = self.episodes.iter().map(|_| Anim::new(0.0, 14.0)).collect();
                    }
                }
                Msg::Similar(v) => { self.focus = v.iter().map(|_| Anim::new(0.0, 14.0)).collect(); self.similar = v; }
                Msg::Toggled => {}
            }
        }
        let n = self.row_items().len();
        if self.col >= n && n > 0 { self.col = n - 1; }
        let mut busy = self.appear.step(cx.dt) | self.backdrop.update(cx);
        let max_off = (n as f32 * (EW + GAP) - (1920.0 - PAD - 60.0)).max(0.0);
        self.sx.set(((self.col as f32 - 1.0).max(0.0) * (EW + GAP)).min(max_off));
        busy |= self.sx.step(cx.dt);
        self.sy.set(if self.zone == Zone::People { 300.0 } else { 0.0 });
        busy |= self.sy.step(cx.dt);
        let np = self.chips().len();
        if self.pcol >= np && np > 0 { self.pcol = np - 1; }
        self.px.set(((self.pcol as f32 - 2.0).max(0.0) * (PW + PGAP)).min((np as f32 * (PW + PGAP) - (1920.0 - 2.0 * PAD)).max(0.0)));
        busy |= self.px.step(cx.dt);
        let col = self.col;
        let zone_row = self.zone == Zone::Row;
        for (i, f) in self.focus.iter_mut().enumerate() { f.set(if zone_row && i == col { 1.0 } else { 0.0 }); busy |= f.step(cx.dt); }
        let btn = self.btn;
        let zb = self.zone == Zone::Buttons;
        for (i, f) in self.bfocus.iter_mut().enumerate() { f.set(if zb && i == btn { 1.0 } else { 0.0 }); busy |= f.step(cx.dt); }
        // the text block shows the focused episode while browsing a season
        let want = if zone_row && self.series_id().is_some() { self.episodes.get(col).cloned() } else { None };
        if want.as_ref().map(|i| &i.id) != self.shown.as_ref().map(|i| &i.id) { self.shown = want; self.shown_a.snap(0.0); self.shown_a.set(1.0); }
        busy | self.shown_a.step(cx.dt)
    }

    fn draw(&mut self, g: &mut Gfx, cx: &mut Cx) {
        let Some(c) = cx.client.clone() else { return };
        background(g, cx.t);
        self.backdrop.draw(g, cx, Rect::new(0.0, 0.0, 1920.0, 1080.0), 1.0);
        g.vgrad(Rect::new(0.0, 560.0, 1920.0, 520.0), Color::hex(0x07080c, 0.0), Color::hex(0x07080c, 0.85), 0.0);
        let a = smooth(self.appear.v);
        g.alpha = a;
        let has_row = !self.row_items().is_empty();
        let text_bottom = if has_row { 560.0 } else { 760.0 } - self.sy.v;

        // text block: the item, or the focused episode
        let it = self.shown.clone().unwrap_or_else(|| self.item.clone());
        let sa = if self.shown.is_some() { smooth(self.shown_a.v) } else { 1.0 };
        let x = PAD;
        let tech = tech_line(&it);
        let prev = g.alpha;
        g.alpha = a * sa;
        let mut y = text_bottom;
        if !tech.is_empty() { g.text(x, y - 26.0, 19.0, W::Medium, FAINT, &tech); y -= 40.0; }
        let ov = it.overview.clone().unwrap_or_default().replace('\n', " ");
        let lines = g.text.wrap(&ov, 23.0, W::Regular, 900.0, if has_row { 3 } else { 5 });
        y -= lines.len() as f32 * 35.0 + 12.0;
        for (i, l) in lines.iter().enumerate() { g.text(x, y + i as f32 * 35.0, 23.0, W::Regular, TEXT.a(0.78), l); }
        y -= 46.0;
        let mut meta = meta_line(&it);
        if !it.is_series() {
            let ticks = it.run_time_ticks.unwrap_or(0) - if it.progress() > 0.0 && !it.played() { it.resume_ticks() } else { 0 };
            if ticks > 0 { meta = format!("{meta}  ·  Ends at {}", hhmm(ticks / crate::jf::TICKS_PER_SEC)); }
        }
        g.text(x, y, 21.0, W::Regular, MUTED, &meta);
        if it.is_episode() {
            y -= 48.0;
            let s = g.text.ellipsize(&it.subtitle(), 30.0, W::Semi, 1100.0);
            g.text(x, y, 30.0, W::Semi, TEXT, &s);
        }
        // logo (or the big title)
        let logo = c.logo(&it, 800);
        let mut drawn = false;
        if let Some((tex, la)) = (!logo.is_empty()).then(|| cx.images.get(&logo)).flatten() {
            let tex = unsafe { &*(tex as *const crate::gfx::Texture) };
            let s = (620.0 / tex.w as f32).min(190.0 / tex.h as f32);
            let (w, h) = (tex.w as f32 * s, tex.h as f32 * s);
            y -= h + 26.0;
            g.image(tex, Rect::new(x, y, w, h), 0.0, la, 0.5, 0.5);
            drawn = true;
        }
        if !drawn {
            let t = it.title();
            let ls = g.text.wrap(&t, 80.0, W::Bold, 1100.0, 2);
            y -= ls.len() as f32 * 84.0 + 20.0;
            for (i, l) in ls.iter().enumerate() { g.text_t(x, y + i as f32 * 84.0, 80.0, W::Bold, TEXT, l, -1.8); }
        }
        g.alpha = prev;

        // actions
        let by = text_bottom + 28.0;
        let mut bx = PAD;
        for (i, (_, label, icon)) in self.buttons().iter().enumerate() {
            let f = self.bfocus.get(i).map_or(0.0, |f| f.v);
            let r = button(g, bx, by, label, Some(*icon), f, i == 0, 72.0);
            cx.hot.push((r, Hot { row: 0, col: i as i32 }));
            bx = r.right() + 18.0;
        }

        // seasons + episodes / similar
        let mut bottom = by + 72.0;
        if has_row {
            let mut ry = by + 112.0;
            if self.seasons.len() > 1 {
                let mut sx = PAD;
                for (i, s) in self.seasons.iter().enumerate() {
                    let sel = i == self.season;
                    let f = if i == self.scol && self.zone == Zone::Seasons { 1.0 } else { 0.0 };
                    let w = g.text.measure(&s.name, 22.0, W::Semi, 0.0) + 36.0;
                    let r = Rect::new(sx, ry, w, 46.0);
                    g.rect(r, if f > 0.0 { Color(1.0, 1.0, 1.0, 1.0) } else if sel { SURFACE_HI } else { Color(0.0, 0.0, 0.0, 0.0) }, 23.0);
                    g.text(sx + 18.0, ry + 11.0, 22.0, W::Semi, if f > 0.0 { BG } else if sel { TEXT } else { MUTED }, &s.name);
                    cx.hot.push((r, Hot { row: 1, col: i as i32 }));
                    sx += w + 8.0;
                    if sx > 1800.0 { break; }
                }
                ry += 66.0;
            } else {
                label(g, PAD, ry + 6.0, if self.series_id().is_some() { "Episodes" } else { "More like this" });
                ry += 48.0;
            }
            let items = self.row_items().clone();
            let sxv = self.sx.v;
            for (i, e) in items.iter().enumerate() {
                let px = PAD + i as f32 * (EW + GAP) - sxv;
                if px > 1920.0 { break; }
                if px + EW < -40.0 { continue; }
                let r = Rect::new(px, ry, EW, EH);
                let f = self.focus.get(i).map_or(0.0, |f| f.v);
                let (t, s) = if e.is_episode() { (format!("{}. {}", e.index_number.unwrap_or(0), e.name), if e.progress() > 0.0 && !e.played() { format!("{} min left", e.minutes_left()) } else { format!("{} min", e.minutes()) }) } else { (e.name.clone(), e.subtitle()) };
                wide_card(g, cx, &c, e, r, f, &t, &s);
                cx.hot.push((r, Hot { row: 2, col: i as i32 }));
            }
            bottom = ry + EH + 76.0; // + the cards' title and subtitle lines
        }

        // genres + cast
        let chips = self.chips();
        if !chips.is_empty() {
            let mut y = bottom + 44.0;
            label(g, PAD, y, "Genres & cast");
            y += 44.0;
            let pf = self.zone == Zone::People;
            let mut x = PAD - self.px.v;
            for (i, ch) in chips.iter().enumerate() {
                let f = if pf && i == self.pcol { 1.0 } else { 0.0 };
                match ch {
                    Chip::Genre(name) => {
                        let w = g.text.measure(name, 22.0, W::Semi, 0.0) + 44.0;
                        let r = Rect::new(x, y + 50.0, w, 54.0);
                        g.rect(r, if f > 0.0 { Color(1.0, 1.0, 1.0, 1.0) } else { SURFACE }, 27.0);
                        g.text(r.x + 22.0, r.y + 15.0, 22.0, W::Semi, if f > 0.0 { BG } else { TEXT }, name);
                        cx.hot.push((r, Hot { row: 3, col: i as i32 }));
                        x += w + PGAP;
                    }
                    Chip::Person(p) => {
                        let r = Rect::new(x + 20.0, y, 130.0, 130.0);
                        if f > 0.0 { focus_ring(g, r, 65.0, f); }
                        picture(g, cx, &c.person_img(p, 260), r, 65.0, &initial(&p.name));
                        let n = g.text.ellipsize(&p.name, 19.0, W::Semi, PW);
                        g.text_c(x + PW / 2.0, y + 142.0, 19.0, W::Semi, if f > 0.0 { TEXT } else { TEXT.a(0.85) }, &n);
                        let role = if p.kind == "Director" { "Director".to_string() } else { p.role.clone().unwrap_or_default() };
                        let rl = g.text.ellipsize(&role, 16.0, W::Regular, PW);
                        g.text_c(x + PW / 2.0, y + 168.0, 16.0, W::Regular, MUTED, &rl);
                        cx.hot.push((Rect::new(x, y, PW, 190.0), Hot { row: 3, col: i as i32 }));
                        x += PW + PGAP;
                    }
                }
                if x > 1920.0 { break; }
            }
        }
        g.alpha = 1.0;
    }
}

/// "1080p · H.264 · E-AC3 5.1 · Subtitles: English, German"
pub fn tech_line(it: &Item) -> String {
    let Some(ms) = it.media_sources.first() else { return String::new() };
    let mut parts = vec![];
    if let Some(v) = ms.media_streams.iter().find(|s| s.kind == "Video") {
        let h = v.height.unwrap_or(0);
        let w = v.width.unwrap_or(0);
        parts.push(if w >= 3200 || h >= 1800 { "4K".into() } else if w >= 1800 || h >= 1000 { "1080p".into() } else if h >= 700 { "720p".into() } else if h > 0 { format!("{h}p") } else { String::new() });
        if let Some(r) = &v.video_range_type { if r != "SDR" { parts.push(r.clone()); } }
        parts.push(v.codec.clone().unwrap_or_default().to_uppercase().replace("HEVC", "HEVC").replace("H264", "H.264"));
    }
    if let Some(a) = ms.media_streams.iter().filter(|s| s.kind == "Audio").min_by_key(|s| !s.is_default) {
        let ch = match a.channels { Some(8) => " 7.1", Some(6) => " 5.1", Some(2) => " Stereo", _ => "" };
        parts.push(format!("{}{ch}", a.codec.clone().unwrap_or_default().to_uppercase()));
    }
    let subs: Vec<String> = ms.media_streams.iter().filter(|s| s.kind == "Subtitle").filter_map(|s| s.language.clone()).take(4).collect();
    if !subs.is_empty() { parts.push(format!("Subtitles: {}", subs.join(", "))); }
    parts.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("  ·  ")
}
