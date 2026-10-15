//! Item page: backdrop, logo/title, metadata, actions; seasons + episodes for shows, similar titles for films;
//! genres + cast at the bottom (each opens a grid of matching titles).
use super::*;
use crate::app::PlaySpec;
use crate::feed::{self, Removal};
use crate::jf::{enc, Item, Person};
use crate::net::Mailbox;

enum Msg { Item(Result<Item, String>), Seasons(Vec<Item>), Episodes(String, Vec<Item>), Similar(Vec<Item>), Toggled, Preview(Result<Removal, String>), Deleted(Result<Removal, String>), CanManage(bool) }

/// The delete confirmation: asking the server what would go, waiting for the answer, deleting, or why it can't.
enum Confirm { Loading, Ready(Removal), Deleting, Failed(String) }

#[derive(Clone, Copy, PartialEq, Debug)]
enum Zone { Buttons, Seasons, Row, People }

#[derive(Clone, Copy, PartialEq)]
enum Btn { Play, Restart, Version, Watched, Favorite, Delete }

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
    /// the server's helper for deleting (feed.rs), once it has said that deleting is switched on there
    manage: Option<String>,
    manage_ok: bool,
    confirm: Option<Confirm>,
    /// 0 = Cancel, 1 = Delete
    cbtn: usize,
    confirm_a: Anim,
    /// actions for the app (after a delete: a toast, then back)
    pending: Vec<Action>,
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
            zone: Zone::Buttons, btn: 0, col: 0, sx: Anim::new(0.0, 10.0), focus: vec![], bfocus: (0..7).map(|_| Anim::new(0.0, 14.0)).collect(),
            backdrop: Backdrop::new(), appear: Anim::new(0.0, 4.0), shown: None, shown_a: Anim::new(1.0, 7.0),
            version: 0, pcol: 0, px: Anim::new(0.0, 10.0), sy: Anim::new(0.0, 8.0),
            manage: cx.client.as_ref().and_then(|c| feed::base(&c.s.server)), manage_ok: false, confirm: None, cbtn: 0, confirm_a: Anim::new(0.0, 9.0), pending: vec![],
        };
        d.appear.set(1.0);
        if let Some(c) = cx.client { d.backdrop.set(c.backdrop(&d.item, 1920)); }
        // the Delete button only where the server has the helper with deleting switched on
        if let (Some(c), Some(base)) = (cx.client.clone(), d.manage.clone()) {
            let tx = d.mb.tx.clone();
            cx.pool.spawn(move || { let ok = feed::storage(&c.agent, &base).map_or(false, |s| s.delete); let _ = tx.send(Msg::CanManage(ok)); crate::net::wake(); });
        }
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
        if self.can_delete() { v.push((Btn::Delete, String::new(), Icon::Trash)); }
        v
    }

    /// A film or a whole series, Jellyfin says this user may delete, and the server has the helper that does it.
    fn can_delete(&self) -> bool {
        (self.item.is_series() || self.item.kind == "Movie") && self.item.can_delete == Some(true) && self.manage_ok
    }

    fn start_delete_check(&mut self, cx: &mut Cx) {
        let (Some(c), Some(base)) = (cx.client.clone(), self.manage.clone()) else { return };
        self.confirm = Some(Confirm::Loading);
        self.cbtn = 0;
        self.confirm_a.set(1.0);
        let (tx, id) = (self.mb.tx.clone(), self.item.id.clone());
        cx.pool.spawn(move || { let _ = tx.send(Msg::Preview(feed::preview(&c.agent, &base, &c.auth(), &id))); crate::net::wake(); });
    }

    fn confirm_key(&mut self, k: Key, cx: &mut Cx) -> Action {
        let deleting = matches!(self.confirm, Some(Confirm::Deleting));
        match k {
            Key::Back => if !deleting { self.confirm = None; },
            Key::Exit => return Action::Exit,
            Key::Left => self.cbtn = 0,
            Key::Right => if matches!(self.confirm, Some(Confirm::Ready(_))) { self.cbtn = 1; },
            Key::Ok => {
                if deleting { return Action::None; }
                if self.cbtn == 1 {
                    if let (Some(Confirm::Ready(r)), Some(c), Some(base)) = (self.confirm.take(), cx.client.clone(), self.manage.clone()) {
                        crate::log!("delete: {} {:?} ({} {}, {} bytes)", self.item.kind, r.title, r.app, r.id, r.size);
                        self.confirm = Some(Confirm::Deleting);
                        let (tx, id, expect) = (self.mb.tx.clone(), self.item.id.clone(), r.id);
                        cx.pool.spawn(move || { let _ = tx.send(Msg::Deleted(feed::delete(&c.agent, &base, &c.auth(), &id, expect))); crate::net::wake(); });
                        return Action::None;
                    }
                }
                self.confirm = None;
            }
            _ => {}
        }
        Action::None
    }

    fn draw_confirm(&mut self, g: &mut Gfx, cx: &mut Cx) {
        let Some(cf) = &self.confirm else { return };
        let a = smooth(self.confirm_a.v);
        g.alpha = a;
        g.rect(Rect::new(0.0, 0.0, 1920.0, 1080.0), Color(0.0, 0.0, 0.0, 0.74), 0.0);
        let p = Rect::new(460.0, 250.0 + (1.0 - a) * 20.0, 1000.0, 560.0);
        g.shadow(p.offset(0.0, 20.0), Color(0.0, 0.0, 0.0, 0.6), 30.0, 50.0);
        g.rect(p, Color::hex(0x161722, 1.0), 30.0);
        let (x, w) = (p.x + 56.0, p.w - 112.0);
        let danger = Color::hex(0xff6b6b, 1.0);
        let title = format!("Delete {}?", self.item.name);
        let t = g.text.ellipsize(&title, 44.0, W::Bold, w);
        g.text_t(x, p.y + 50.0, 44.0, W::Bold, TEXT, &t, -0.8);
        let mut y = p.y + 130.0;
        match cf {
            Confirm::Loading => { spinner(g, x + 18.0, y + 30.0, 16.0, cx.t, MUTED); g.text(x + 54.0, y + 16.0, 24.0, W::Regular, MUTED, "Checking with Sonarr / Radarr…"); }
            Confirm::Deleting => { spinner(g, x + 18.0, y + 30.0, 16.0, cx.t, MUTED); g.text(x + 54.0, y + 16.0, 24.0, W::Regular, MUTED, "Deleting…"); }
            Confirm::Failed(e) => {
                g.text(x, y, 24.0, W::Semi, danger, "This can't be deleted from the TV");
                let msg: String = e.chars().take(1).flat_map(|c| c.to_uppercase()).chain(e.chars().skip(1)).collect();
                for (i, l) in g.text.wrap(&msg, 23.0, W::Regular, w, 4).iter().enumerate() { g.text(x, y + 46.0 + i as f32 * 34.0, 23.0, W::Regular, MUTED, l); }
            }
            Confirm::Ready(r) => {
                // exactly what Sonarr/Radarr will delete, under their own name
                let arr = if r.app == "sonarr" { "Sonarr" } else { "Radarr" };
                let name = format!("{}{}  ·  in {arr}", r.title, r.year.map(|y| format!(" ({y})")).unwrap_or_default());
                let n = g.text.ellipsize(&name, 26.0, W::Semi, w);
                g.text(x, y, 26.0, W::Semi, TEXT, &n);
                let what = if r.app == "sonarr" {
                    let seasons = if r.seasons > 0 { format!("{} season{}  ·  ", r.seasons, if r.seasons == 1 { "" } else { "s" }) } else { String::new() };
                    format!("The whole series: {seasons}{} episode file{}  ·  {}", r.files, if r.files == 1 { "" } else { "s" }, feed::size(r.size))
                } else { format!("The film  ·  {}", feed::size(r.size)) };
                g.text(x, y + 40.0, 22.0, W::Medium, MUTED, &what);
                let body = format!("The files are deleted from the server and {arr} stops downloading it. This can't be undone.");
                for (i, l) in g.text.wrap(&body, 22.0, W::Regular, w, 2).iter().enumerate() { g.text(x, y + 84.0 + i as f32 * 32.0, 22.0, W::Regular, MUTED, l); }
                y += 186.0;
                if let Some(v) = r.volumes.first().filter(|v| v.total > 0) {
                    // the disk: used, with the part this frees marked
                    let used = v.total.saturating_sub(v.free);
                    let bar = Rect::new(x, y + 44.0, w, 14.0);
                    g.rect(bar, Color(1.0, 1.0, 1.0, 0.12), 7.0);
                    let uw = bar.w * used as f32 / v.total as f32;
                    let fw = (bar.w * r.size.min(used) as f32 / v.total as f32).max(4.0);
                    g.rect(Rect::new(bar.x, bar.y, uw, bar.h), Color(1.0, 1.0, 1.0, 0.55), 7.0);
                    g.rect(Rect::new(bar.x + uw - fw, bar.y, fw, bar.h), danger.a(0.9 + 0.1 * (cx.t * 3.0).sin()), 7.0);
                    g.text(x, y, 22.0, W::Medium, TEXT, &format!("Free space: {} now  →  {} afterwards", feed::size(v.free), feed::size(v.free + r.size)));
                    g.text_r(x + w, y + 3.0, 19.0, W::Regular, FAINT, &format!("of {}", feed::size(v.total)));
                }
            }
        }
        // buttons: Cancel first (and focused first), Delete in red
        let by = p.bottom() - 112.0;
        let can = matches!(cf, Confirm::Ready(_));
        let fc = if self.cbtn == 0 { 1.0 } else { 0.0 };
        let r0 = button(g, x, by, if matches!(cf, Confirm::Failed(_)) { "Close" } else { "Cancel" }, None, fc, false, 68.0);
        cx.hot.push((r0, Hot { row: 9, col: 0 }));
        if can || matches!(cf, Confirm::Deleting) {
            let label = "Delete";
            let tw = g.text.measure(label, 24.0, W::Semi, 0.0);
            let r1 = Rect::new(r0.right() + 18.0, by, tw + 26.0 + 14.0 + 64.0, 68.0);
            let f = if self.cbtn == 1 { 1.0 } else { 0.0 };
            if f > 0.0 { g.shadow(r1.offset(0.0, 10.0), danger.a(0.35), 34.0, 26.0); }
            g.rect(r1, if f > 0.0 { danger } else { danger.a(0.16) }, 34.0);
            let ink = if f > 0.0 { Color::hex(0x1a0505, 1.0) } else { danger };
            draw_icon(g, Icon::Trash, r1.x + 32.0, by + 21.0, 26.0, ink);
            g.text(r1.x + 72.0, by + (68.0 - 28.8) / 2.0, 24.0, W::Semi, ink, label);
            cx.hot.push((r1, Hot { row: 9, col: 1 }));
        }
        g.alpha = 1.0;
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
            Btn::Delete => { self.start_delete_check(cx); Action::None }
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

    fn poll_action(&mut self) -> Action { if self.pending.is_empty() { Action::None } else { self.pending.remove(0) } }

    fn key(&mut self, k: Key, cx: &mut Cx) -> Action {
        if self.confirm.is_some() { return self.confirm_key(k, cx); }
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
        if self.confirm.is_some() {
            if h.row != 9 { return false; }
            let ch = self.cbtn != h.col as usize;
            self.cbtn = h.col as usize;
            return ch;
        }
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
                Msg::CanManage(ok) => self.manage_ok = ok,
                Msg::Preview(r) => if matches!(self.confirm, Some(Confirm::Loading)) {
                    self.confirm = Some(match r { Ok(r) => Confirm::Ready(r), Err(e) => Confirm::Failed(e) });
                    self.cbtn = 0;
                },
                Msg::Deleted(r) => match r {
                    Ok(r) => {
                        crate::log!("delete: done ({})", r.title);
                        crate::jf::mark_deleted(&self.item.id);
                        let free = r.volumes.first().map(|v| format!("  ·  {} free", feed::size(v.free + r.size))).unwrap_or_default();
                        self.confirm = None;
                        self.pending.push(Action::Toast(format!("Deleted {}{free}", r.title)));
                        self.pending.push(Action::Pop);
                    }
                    Err(e) => { crate::log!("delete: failed: {e}"); self.confirm = Some(Confirm::Failed(e)); self.cbtn = 0; }
                },
            }
        }
        let n = self.row_items().len();
        if self.col >= n && n > 0 { self.col = n - 1; }
        if self.confirm.is_none() { self.confirm_a.snap(0.0); }
        let mut busy = self.appear.step(cx.dt) | self.backdrop.update(cx) | self.confirm_a.step(cx.dt) | self.confirm.is_some();
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
        if self.confirm.is_some() {
            // the dialog owns the pointer: nothing behind it is clickable
            cx.hot.clear();
            self.draw_confirm(g, cx);
        }
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
