//! Playback screen: chooses how to play (direct, or the server converting the whole stream as a fallback),
//! drives the engine, reports progress to Jellyfin, and draws the OSD, track menu, subtitles, skip-intro and up-next.
use super::*;
use crate::app::{prefs, PlaySpec, Start};
use crate::gfx::Texture;
use crate::jf::{fmt_time, Client, Item, MediaSource, MediaStream, Segment, TICKS_PER_SEC};
use crate::net::Mailbox;
use crate::player::engine::{Phase, Request, SubEvent};
use crate::player::Player;
use serde_json::json;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

enum Msg { Item(Result<Item, String>), Segments(Vec<Segment>), Srt(i32, Result<String, String>), Queue(Vec<Item>) }

#[derive(Clone, Copy, PartialEq, Debug)]
enum Method { Direct, Convert }

#[derive(Clone)]
struct Cue { start: i64, end: i64, lines: Vec<String> }

struct Menu { idx: usize }

pub struct PlayerUi {
    c: Client,
    pool: Arc<Pool>,
    mb: Mailbox<Msg>,
    spec: PlaySpec,
    item: Option<Item>,
    ms: Option<MediaSource>,
    audio: Option<i32>,
    sub: Option<i32>,
    method: Method,
    session: String,
    reported_start: bool,
    last_report: Instant,
    osd: Anim,
    osd_until: Instant,
    seek: Option<(i64, Instant, u32)>,
    menu: Option<Menu>,
    menu_a: Anim,
    cues: Vec<Cue>,
    /// cues of every embedded text track, collected by the engine as it demuxes (keyed by Jellyfin's stream index)
    local_cues: std::collections::HashMap<i32, Vec<Cue>>,
    img_q: VecDeque<SubEvent>,
    img_cur: Option<(i64, Option<i64>, (i32, i32), Vec<(Option<Texture>, crate::player::engine::SubRect)>)>,
    garbage: Vec<Texture>,
    shown_cue: Option<usize>,
    /// per-start id for server conversions: Jellyfin reuses a running job (from its old start time) for a known id
    stream_sid: String,
    segments: Vec<Segment>,
    upnext_dismissed: bool,
    upnext_at: Option<Instant>,
    done: bool,
    suspended_at: Option<i64>,
    last_pos: i64,
    flash: Option<(Icon, Instant)>,
    error: Option<String>,
    fell_back: bool,
    started_at: Instant,
    ended_handled: bool,
    /// paused when the app went to the background (e.g. LG's screensaver): pause again once the picture is back
    pause_on_picture: bool,
    /// the TV's sound goes to Bluetooth (checked every few seconds): audio is played earlier by the lip-sync lead
    bt: bool,
    bt_checked: Instant,
    /// a lip-sync change waiting for the key presses to settle before it reloads the pipeline
    lead_pending: Option<Instant>,
}

/// Lip-sync steps and range for Bluetooth headphones, in ms.
const LEAD_STEP: i32 = 25;
const LEAD_MAX: i32 = 500;
/// subtitle timing (prefs.sub_delay_ms): steps and range, ms
const SUB_DELAY_STEP: i32 = 50;
const SUB_DELAY_MAX: i32 = 1000;

// ---- remembered track choices, per series (by language/title, so they carry over to the next episode)
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug)]
struct TrackPick { lang: String, title: String, forced: bool }
#[derive(serde::Serialize, serde::Deserialize, Clone, Default, Debug)]
#[serde(default)]
struct Remembered { audio: Option<TrackPick>, /** Some(None) = subtitles off */ sub: Option<Option<TrackPick>> }

fn memory_path() -> String { format!("{}/tracks.json", crate::jf::DATA_DIR) }
fn memory_all() -> std::collections::HashMap<String, Remembered> {
    std::fs::read_to_string(memory_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}
fn pick_of(s: &MediaStream) -> TrackPick { TrackPick { lang: s.language.clone().unwrap_or_default(), title: s.title.clone().unwrap_or_default(), forced: s.is_forced } }
/// The stream that best matches a remembered pick: same language + forced flag, preferring the same title.
fn best_match<'a>(streams: &[&'a MediaStream], p: &TrackPick) -> Option<&'a MediaStream> {
    let lang = |s: &&&MediaStream| s.language.clone().unwrap_or_default() == p.lang;
    streams.iter().filter(lang).filter(|s| s.is_forced == p.forced).find(|s| s.title.clone().unwrap_or_default() == p.title)
        .or_else(|| streams.iter().filter(lang).find(|s| s.is_forced == p.forced))
        .or_else(|| streams.iter().find(lang)).copied()
}

fn video_ok(s: &MediaStream) -> bool { matches!(s.codec.as_deref().unwrap_or("").to_ascii_lowercase().as_str(), "h264" | "hevc" | "h265" | "vp9" | "av1") }
fn image_sub(s: &MediaStream) -> bool { matches!(s.codec.as_deref().unwrap_or("").to_ascii_lowercase().as_str(), "pgssub" | "dvdsub" | "dvd_subtitle" | "hdmv_pgs_subtitle" | "dvbsub") }
/// Jellyfin numbers external subtitle files ahead of the embedded streams; FFmpeg only sees the file.
fn file_index(ms: &MediaSource, jf_index: i32) -> i32 {
    jf_index - ms.media_streams.iter().filter(|s| s.is_external && s.index < jf_index).count() as i32
}
/// The other way round: the file's stream index -> Jellyfin's index (embedded streams only).
fn jf_index(ms: &MediaSource, file_idx: i32) -> Option<i32> {
    ms.media_streams.iter().find(|s| !s.is_external && file_index(ms, s.index) == file_idx).map(|s| s.index)
}
fn stream_label(s: &MediaStream) -> String {
    s.display_title.clone().or_else(|| s.title.clone()).or_else(|| s.language.clone()).unwrap_or_else(|| format!("Track {}", s.index))
}

impl PlayerUi {
    pub fn start(spec: PlaySpec, c: Client, _player: &mut Player, pool: &Arc<Pool>) -> PlayerUi {
        let p = PlayerUi {
            c, pool: pool.clone(), mb: Mailbox::new(), spec, item: None, ms: None, audio: None, sub: None, method: Method::Direct,
            session: session_id(), reported_start: false, last_report: Instant::now(), osd: Anim::new(1.0, 8.0), osd_until: Instant::now() + Duration::from_secs(5),
            seek: None, menu: None, menu_a: Anim::new(0.0, 12.0), cues: vec![], local_cues: Default::default(), img_q: VecDeque::new(), img_cur: None, segments: vec![],
            upnext_dismissed: false, upnext_at: None, done: false, suspended_at: None, last_pos: 0, flash: None, error: None, fell_back: false,
            started_at: Instant::now(), ended_handled: false, pause_on_picture: false, garbage: vec![], shown_cue: None, stream_sid: String::new(),
            bt: crate::player::bluetooth_audio(), bt_checked: Instant::now(), lead_pending: None,
        };
        if p.bt { crate::log!("player: sound goes to Bluetooth, lip sync {} ms", prefs().bt_audio_lead_ms); }
        // always fetch the full item: rows don't carry media sources
        let (c1, tx, id) = (p.c.clone(), p.mb.tx.clone(), p.spec.item.id.clone());
        let tx2 = tx.clone();
        let c2 = p.c.clone();
        let id2 = id.clone();
        p.pool.spawn(move || { let _ = tx.send(Msg::Item(c1.item(&id))); crate::net::wake(); });
        p.pool.spawn(move || { let _ = tx2.send(Msg::Segments(c2.segments(&id2))); crate::net::wake(); });
        p
    }

    pub fn finished(&self) -> bool { self.done }
    pub fn video_visible(&self, player: &Player) -> bool { player.active() || self.suspended_at.is_some() || self.item.is_none() }
    pub fn wants_ticks(&self) -> bool {
        self.osd.v > 0.01 || self.seek.is_some() || self.menu.is_some() || !self.cues.is_empty() || !self.img_q.is_empty() || self.img_cur.is_some()
            || self.upnext_at.is_some() || self.in_skippable().is_some() || self.flash.is_some()
    }

    fn show_osd(&mut self, secs: u64) { self.osd.set(1.0); self.osd_until = Instant::now() + Duration::from_secs(secs); }

    fn start_ns(&self) -> i64 {
        match self.spec.start {
            Start::Begin => 0,
            Start::Resume => {
                let it = self.item.as_ref().unwrap_or(&self.spec.item);
                if it.progress() > 0.0 && !it.played() { (it.resume_ticks() * 100 - 2_000_000_000).max(0) } else { 0 }
            }
        }
    }

    /// Pick audio + subtitle tracks and how to play, then start the engine at `pos_ns`.
    /// Tracks: what was picked last time in this series, else the app's audio language, else the server's defaults
    /// (which already follow the Jellyfin account's language and subtitle-mode settings).
    fn choose_tracks(&mut self) {
        let Some(ms) = self.ms.clone() else { return };
        let p = prefs();
        let mem = if p.remember_tracks { self.memory_key().and_then(|k| memory_all().remove(&k)) } else { None };
        let audios: Vec<&MediaStream> = ms.media_streams.iter().filter(|s| s.kind == "Audio").collect();
        let pref_lang = |s: &&&MediaStream| !p.audio_lang.is_empty() && s.language.as_deref() == Some(p.audio_lang.as_str());
        let def = ms.default_audio_stream_index;
        // any codec works: what the TV can't decode itself is re-encoded on the TV (player::audio)
        let remembered = mem.as_ref().and_then(|m| m.audio.as_ref()).and_then(|a| best_match(&audios, a));
        let pick = remembered.or_else(|| audios.iter().find(pref_lang).copied()).or_else(|| audios.iter().find(|s| Some(s.index) == def).copied())
            .or_else(|| audios.iter().find(|s| s.is_default).copied()).or(audios.first().copied());
        self.audio = pick.map(|s| s.index);
        let subs: Vec<&MediaStream> = ms.media_streams.iter().filter(|s| s.kind == "Subtitle").collect();
        self.sub = match mem.as_ref().and_then(|m| m.sub.clone()) {
            Some(None) => None,
            Some(Some(want)) => best_match(&subs, &want).map(|s| s.index),
            None if p.subs_auto => ms.default_subtitle_stream_index.filter(|&i| i >= 0 && subs.iter().any(|s| s.index == i)),
            None => None,
        };
        crate::log!("tracks: audio={:?} sub={:?} (remembered: {})", self.audio, self.sub, mem.is_some());
    }

    /// Series id for episodes (choices carry over to the next episode); films don't remember.
    fn memory_key(&self) -> Option<String> {
        let it = self.item.as_ref().unwrap_or(&self.spec.item);
        if it.is_episode() { it.series_id.clone() } else { None }
    }

    fn remember(&self) {
        if !prefs().remember_tracks { return; }
        let (Some(k), Some(ms)) = (self.memory_key(), self.ms.as_ref()) else { return };
        let find = |i: Option<i32>| i.and_then(|i| ms.media_streams.iter().find(|s| s.index == i)).map(pick_of);
        let mut all = memory_all();
        all.insert(k, Remembered { audio: find(self.audio), sub: Some(find(self.sub)) });
        if let Ok(s) = serde_json::to_string(&all) { let _ = std::fs::write(memory_path(), s); }
    }

    fn chapters(&self) -> Vec<(i64, String)> {
        let it = self.item.as_ref().unwrap_or(&self.spec.item);
        it.chapters.iter().enumerate().map(|(i, c)| (c.start_position_ticks * 100, c.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| format!("Chapter {}", i + 1)))).collect()
    }

    /// The track menu: section headers (None) and selectable entries (kind 'a' audio / 's' subtitles, Jellyfin index,
    /// -1 = off; 'c' chapter number).
    fn entries(&self) -> Vec<(String, Option<(char, i32)>)> {
        let mut v = menu_entries(self.ms.as_ref());
        if self.sub.is_some() {
            let ms = prefs().sub_delay_ms;
            v.push(("Subtitle timing".into(), None));
            v.push((match ms { 0 => "As in the file   ◀ ▶".into(), m if m > 0 => format!("{m} ms later   ◀ ▶"), m => format!("{} ms earlier   ◀ ▶", -m) }, Some(('t', 0))));
        }
        if self.bt {
            let ms = prefs().bt_audio_lead_ms;
            v.push(("Bluetooth lip sync".into(), None));
            v.push((if ms == 0 { "Off   ◀ ▶".into() } else { format!("Sound {ms} ms earlier   ◀ ▶") }, Some(('d', 0))));
        }
        let ch = self.chapters();
        if ch.len() > 1 {
            v.push(("Chapters".into(), None));
            for (i, (t, name)) in ch.iter().enumerate() { v.push((format!("{}   {name}", fmt_time(*t)), Some(('c', i as i32)))); }
        }
        v
    }

    /// The chapter `d` steps from the current one (-1: back to this chapter's start unless just past it).
    fn chapter_jump(&mut self, player: &mut Player, d: i32) -> bool {
        let ch = self.chapters();
        if ch.len() < 2 { return false; }
        let pos = player.position_ns();
        let cur = ch.iter().rposition(|(t, _)| *t <= pos + 500_000_000).unwrap_or(0) as i32;
        let target = if d < 0 && pos - ch[cur as usize].0 > 3_000_000_000 { cur } else { cur + d };
        let Some((t, name)) = ch.get(target.clamp(0, ch.len() as i32 - 1) as usize).cloned() else { return false };
        self.seek_to(player, t);
        self.show_osd(3);
        crate::log!("chapter: {name}");
        true
    }

    fn launch(&mut self, player: &mut Player, pos_ns: i64) {
        let Some(ms) = self.ms.clone() else { return };
        let video = ms.media_streams.iter().find(|s| s.kind == "Video");
        self.method = if self.fell_back || video.map_or(false, |v| !video_ok(v)) { Method::Convert } else { Method::Direct };
        let id = self.item.as_ref().map(|i| i.id.clone()).unwrap_or_default();
        let sub_stream = self.sub.and_then(|i| ms.media_streams.iter().find(|s| s.index == i && s.kind == "Subtitle")).filter(|s| image_sub(s) && !s.is_external).map(|s| s.index);
        let base = &self.c.s.server;
        if self.method != Method::Direct {
            if !self.stream_sid.is_empty() { let (c, old) = (self.c.clone(), self.stream_sid.clone()); self.pool.spawn(move || c.stop_transcode(&old)); }
            self.stream_sid = session_id();
        }
        let sid = if self.method == Method::Direct { self.session.clone() } else { self.stream_sid.clone() };
        let common = format!("MediaSourceId={}&PlaySessionId={sid}&DeviceId={}", ms.id, self.c.s.device_id);
        let ticks = pos_ns / 100;
        let (url, seekable, offset) = match self.method {
            Method::Direct => (format!("{base}/Videos/{id}/stream?static=true&{common}"), true, 0),
            Method::Convert => (format!("{base}/Videos/{id}/stream.mkv?Static=false&{common}&VideoCodec=h264&AudioCodec=aac&AudioStreamIndex={}&MaxWidth=1920&VideoBitrate=14000000&AudioBitrate=320000&AudioChannels=2&StartTimeTicks={ticks}&Container=mkv",
                self.audio.unwrap_or(-1)), false, pos_ns),
        };
        crate::log!("play {:?} at {} ms audio={:?} sub={:?}", self.method, pos_ns / 1_000_000, self.audio, self.sub);
        self.img_q.clear();
        self.img_cur = None;
        player.play(Request {
            url, auth: self.c.auth(), seekable, start_ns: pos_ns, offset_ns: offset,
            video_stream: if self.method == Method::Direct { video.map(|v| file_index(&ms, v.index)) } else { None },
            audio_stream: if self.method == Method::Direct { self.audio.map(|a| file_index(&ms, a)) } else { None },
            sub_stream: if self.method == Method::Direct { sub_stream.map(|i| file_index(&ms, i)) } else { None },
            window_id: String::new(),
            audio_lead_ns: self.lead_ns(),
        });
        self.load_text_subs();
    }

    /// The position subtitles are timed on: the smooth picture clock, minus the subtitle timing setting.
    fn sub_clock(&self, player: &Player) -> i64 { player.clock_ns() - prefs().sub_delay_ms.clamp(-SUB_DELAY_MAX, SUB_DELAY_MAX) as i64 * 1_000_000 }

    fn lead_ns(&self) -> i64 { if self.bt { prefs().bt_audio_lead_ms.clamp(0, LEAD_MAX) as i64 * 1_000_000 } else { 0 } }

    /// An embedded text track while playing the file directly: its cues come from the engine, not from the server.
    fn sub_is_local(&self, i: i32) -> bool {
        self.method == Method::Direct && self.ms.as_ref().and_then(|ms| ms.media_streams.iter().find(|s| s.index == i)).map_or(false, |s| !s.is_external && !image_sub(s))
    }

    fn load_text_subs(&mut self) {
        self.cues.clear();
        let (Some(i), Some(ms), Some(it)) = (self.sub, self.ms.clone(), self.item.clone()) else { return };
        let Some(s) = ms.media_streams.iter().find(|s| s.index == i) else { return };
        if image_sub(s) { return; }
        if self.sub_is_local(i) { self.cues = self.local_cues.get(&i).cloned().unwrap_or_default(); return; }
        let (c, tx, src) = (self.c.clone(), self.mb.tx.clone(), ms.id.clone());
        self.pool.spawn(move || { let _ = tx.send(Msg::Srt(i, c.subtitle_srt(&it.id, &src, i))); crate::net::wake(); });
    }

    fn report(&mut self, player: &Player, what: &'static str, event: &str) {
        let Some(it) = &self.item else { return };
        let pos = player.position_ns().max(0);
        let body = json!({
            "ItemId": it.id, "MediaSourceId": self.ms.as_ref().map(|m| m.id.clone()), "PlaySessionId": self.session,
            "PositionTicks": pos / 100, "IsPaused": player.paused(), "IsMuted": false, "CanSeek": true, "EventName": event,
            "PlayMethod": if self.method == Method::Direct { "DirectPlay" } else { "Transcode" },
            "AudioStreamIndex": self.audio, "SubtitleStreamIndex": self.sub.unwrap_or(-1),
        });
        let c = self.c.clone();
        self.pool.spawn(move || c.report(what, body));
        self.last_report = Instant::now();
    }

    fn duration_ns(&self, player: &Player) -> i64 {
        let d = self.item.as_ref().and_then(|i| i.run_time_ticks).unwrap_or(0) * 100;
        if d > 0 { d } else { player.duration_ns() }
    }

    fn in_skippable(&self) -> Option<&Segment> {
        let pos = self.last_pos / 100;
        self.segments.iter().find(|s| matches!(s.kind.as_str(), "Intro" | "Recap" | "Preview") && pos >= s.start_ticks && pos < s.end_ticks - 2 * TICKS_PER_SEC)
    }

    fn seek_to(&mut self, player: &mut Player, target: i64) {
        let dur = self.duration_ns(player);
        let t = target.clamp(0, (dur - 3_000_000_000).max(0));
        self.launch(player, t);
        self.last_pos = t;
        self.upnext_at = None;
        self.upnext_dismissed = false;
    }

    fn next_spec(&self) -> Option<PlaySpec> {
        let mut q = self.spec.queue.clone();
        if q.is_empty() { return None; }
        let n = q.remove(0);
        Some(PlaySpec::from_start(n).with_queue(q))
    }

    /// Report the end and release the server's transcode (if any).
    pub fn on_exit(&mut self, _client: &Option<Client>) {
        if self.item.is_some() && self.reported_start {
            let it = self.item.clone().unwrap();
            let body = json!({"ItemId": it.id, "MediaSourceId": self.ms.as_ref().map(|m| m.id.clone()), "PlaySessionId": self.session, "PositionTicks": self.last_pos / 100});
            let c = self.c.clone();
            let ses = self.stream_sid.clone();
            let conv = self.method != Method::Direct && !ses.is_empty();
            self.pool.spawn(move || { c.report("stop", body); if conv { c.stop_transcode(&ses); } });
            self.reported_start = false;
        }
    }

    fn exit(&mut self, player: &mut Player) { self.last_pos = player.position_ns().max(self.last_pos); player.stop(); self.done = true; }

    pub fn suspend(&mut self, player: &mut Player, client: &Option<Client>) {
        if self.done || self.suspended_at.is_some() { return; }
        self.last_pos = player.position_ns().max(0);
        self.suspended_at = Some(self.last_pos);
        self.pause_on_picture = player.paused();
        player.shutdown();
        self.on_exit(client);
        crate::log!("player: suspended at {} ms", self.last_pos / 1_000_000);
    }

    pub fn resume_after_background(&mut self, player: &mut Player) {
        if let Some(pos) = self.suspended_at.take() {
            self.session = session_id();
            self.reported_start = false;
            self.started_at = Instant::now();
            self.launch(player, pos);
            self.show_osd(4);
        }
    }

    pub fn key(&mut self, k: Key, player: &mut Player, _cx: &mut Cx) -> Action {
        // an error is up: OK tries again, Back leaves
        if self.error.is_some() && k == Key::Ok { self.retry(player); return Action::None; }
        // track menu
        if self.menu.is_some() {
            let entries = self.entries();
            let m = self.menu.as_mut().unwrap();
            // the lip-sync entry: Left/Right change it (applied once the presses settle), OK does nothing
            if entries.get(m.idx).and_then(|e| e.1).map_or(false, |e| e.0 == 'd') && matches!(k, Key::Left | Key::Right | Key::Ok) {
                if k != Key::Ok {
                    let mut p = prefs();
                    p.bt_audio_lead_ms = (p.bt_audio_lead_ms + if k == Key::Left { -LEAD_STEP } else { LEAD_STEP }).clamp(0, LEAD_MAX);
                    crate::app::set_prefs(p);
                    self.lead_pending = Some(Instant::now());
                }
                return Action::None;
            }
            // subtitle timing: Left/Right change it, applied at once (no restart needed)
            if entries.get(m.idx).and_then(|e| e.1).map_or(false, |e| e.0 == 't') && matches!(k, Key::Left | Key::Right | Key::Ok) {
                if k != Key::Ok {
                    let mut p = prefs();
                    p.sub_delay_ms = (p.sub_delay_ms + if k == Key::Left { -SUB_DELAY_STEP } else { SUB_DELAY_STEP }).clamp(-SUB_DELAY_MAX, SUB_DELAY_MAX);
                    crate::log!("player: subtitles {} ms later", p.sub_delay_ms);
                    crate::app::set_prefs(p);
                    self.shown_cue = None;
                }
                return Action::None;
            }
            match k {
                Key::Back | Key::Left => { self.menu = None; }
                Key::Up => { let mut i = m.idx; while i > 0 { i -= 1; if entries[i].1.is_some() { m.idx = i; break; } } }
                Key::Down => { let mut i = m.idx; while i + 1 < entries.len() { i += 1; if entries[i].1.is_some() { m.idx = i; break; } } }
                Key::Ok => {
                    if let Some((kind, idx)) = entries.get(m.idx).and_then(|e| e.1) {
                        self.menu = None;
                        let pos = player.position_ns();
                        if kind == 'c' { if let Some((t, _)) = self.chapters().get(idx as usize).cloned() { self.seek_to(player, t); self.show_osd(3); } }
                        else if kind == 'a' { if self.audio != Some(idx) { self.audio = Some(idx); self.remember(); self.launch(player, pos); } }
                        else {
                            let new = if idx < 0 { None } else { Some(idx) };
                            if new != self.sub {
                                let needs_reload = |i: Option<i32>, ms: &Option<MediaSource>| i.and_then(|i| ms.as_ref()?.media_streams.iter().find(|s| s.index == i).cloned()).map_or(false, |s| image_sub(&s));
                                let reload = needs_reload(new, &self.ms) || needs_reload(self.sub, &self.ms);
                                self.sub = new;
                                self.remember();
                                if reload { self.launch(player, pos); } else { self.load_text_subs(); }
                            }
                        }
                    }
                }
                _ => {}
            }
            return Action::None;
        }
        // up next card
        if self.upnext_at.is_some() && !self.upnext_dismissed {
            match k {
                Key::Ok => { if let Some(n) = self.next_spec() { self.exit(player); return Action::Play(n); } }
                Key::Back => { self.upnext_dismissed = true; self.upnext_at = None; return Action::None; }
                _ => {}
            }
        }
        let osd_hidden = self.osd.to < 0.5;
        match k {
            Key::Back => {
                if self.seek.is_some() { self.seek = None; return Action::None; }
                self.exit(player); return Action::None;
            }
            Key::Stop | Key::Exit => { self.exit(player); if k == Key::Exit { return Action::Exit; } }
            Key::Ok => {
                if let Some(s) = self.in_skippable().cloned() { if osd_hidden { self.seek_to(player, s.end_ticks * 100); self.show_osd(2); return Action::None; } }
                if let Some((t, _, _)) = self.seek.take() { self.seek_to(player, t); self.show_osd(3); return Action::None; }
                self.toggle_pause(player);
            }
            Key::Play => { if player.paused() { self.toggle_pause(player); } else { self.show_osd(3); } }
            Key::Pause => { if !player.paused() { self.toggle_pause(player); } }
            Key::Left | Key::Right | Key::Ff | Key::Rw => {
                let dir = if matches!(k, Key::Left | Key::Rw) { -1 } else { 1 };
                let base = if matches!(k, Key::Ff | Key::Rw) { 30 } else { 10 };
                let (cur, n) = match self.seek { Some((t, _, n)) => (t, n + 1), None => (player.position_ns(), 1) };
                // holding the button speeds up: 10 s steps, then 30 s, then 60 s
                let step = if n > 12 { 60.max(base) } else if n > 5 { 30.max(base) } else { base } as i64 * 1_000_000_000;
                let dur = self.duration_ns(player);
                let t = (cur + dir * step).clamp(0, (dur - 3_000_000_000).max(0));
                self.seek = Some((t, Instant::now(), n));
                self.show_osd(4);
            }
            Key::Up | Key::Info | Key::Blue => {
                let entries = self.entries();
                let cur = entries.iter().position(|e| e.1 == self.audio.map(|a| ('a', a))).or_else(|| entries.iter().position(|e| e.1.is_some())).unwrap_or(0);
                self.menu = Some(Menu { idx: cur });
            }
            Key::Down => self.show_osd(4),
            // CH+/CH-: chapters when the file has them, else CH+ = next episode
            Key::ChUp => { if !self.chapter_jump(player, 1) { if let Some(n) = self.next_spec() { self.exit(player); return Action::Play(n); } } }
            Key::ChDown => { self.chapter_jump(player, -1); }
            _ => self.show_osd(3),
        }
        Action::None
    }

    /// The app's OLED idle screen may take over: nothing is moving (paused, an error, or finished). Loading the next
    /// episode is not idle.
    pub fn idle_ok(&self, player: &Player) -> bool { player.paused() || self.error.is_some() || self.done }
    /// What the idle screen says under its clock.
    pub fn idle_line(&self) -> String {
        let it = self.item.as_ref().unwrap_or(&self.spec.item);
        format!("Paused  ·  {}", if it.is_episode() { format!("{} {}", it.title(), it.episode_code()) } else { it.title() })
    }

    fn retry(&mut self, player: &mut Player) {
        crate::log!("retry after: {:?}", self.error);
        self.error = None;
        self.fell_back = false;
        if self.item.is_none() {
            let (c, tx, id) = (self.c.clone(), self.mb.tx.clone(), self.spec.item.id.clone());
            self.pool.spawn(move || { let _ = tx.send(Msg::Item(c.item(&id))); crate::net::wake(); });
        } else {
            let p = self.last_pos;
            self.launch(player, p);
        }
    }

    /// Magic Remote pointer. Moves focus menu entries; a click acts on the control under it. Returns a key to
    /// play through `key` for clicks that mean the same as a remote button.
    pub fn pointer(&mut self, x: f32, click: bool, hit: Option<Hot>, player: &mut Player) -> Option<Key> {
        if let (Some(h), Some(m)) = (hit, self.menu.as_mut()) { if h.row == 9 { m.idx = h.col as usize; } }
        if self.menu.is_none() { self.show_osd(if player.paused() { 3600 } else { 4 }); }
        if !click { return None; }
        let pos = player.position_ns();
        match hit.map(|h| (h.row, h.col)) {
            Some((9, _)) | Some((23, _)) | Some((24, _)) => return Some(Key::Ok),
            Some((20, _)) => {
                let f = ((x - PAD) / (1920.0 - 2.0 * PAD)).clamp(0.0, 1.0) as f64;
                let t = (self.duration_ns(player) as f64 * f) as i64;
                self.seek = None;
                self.seek_to(player, t);
                self.show_osd(3);
            }
            Some((21, 0)) => { self.seek_to(player, (pos - 10_000_000_000).max(0)); self.show_osd(3); }
            Some((21, 1)) => self.toggle_pause(player),
            Some((21, 2)) => { self.seek_to(player, pos + 10_000_000_000); self.show_osd(3); }
            Some((21, 3)) => return Some(Key::Up),
            Some((21, 4)) => { self.chapter_jump(player, 1); }
            Some((22, _)) => { if let Some(s) = self.in_skippable().cloned() { self.seek_to(player, s.end_ticks * 100); self.show_osd(2); } }
            _ => {
                if self.menu.is_some() { self.menu = None; }
                else if self.osd.v < 0.5 { self.show_osd(4); }
                else { self.toggle_pause(player); }
            }
        }
        None
    }

    /// Trickplay thumbnail for a position: (sprite url, column, row, thumb w, h), if the server made them.
    fn trickplay_at(&self, t_ns: i64) -> Option<(String, i32, i32, i32, i32)> {
        let it = self.item.as_ref()?;
        let ms = self.ms.as_ref()?;
        let res = it.trickplay.get(&ms.id)?;
        let tp = res.values().min_by_key(|t| (t.width - 320).abs())?;
        if tp.interval <= 0 || tp.tile_width <= 0 || tp.tile_height <= 0 { return None; }
        let idx = ((t_ns / 1_000_000) / tp.interval as i64).min(tp.thumbnail_count.max(1) as i64 - 1) as i32;
        let per = tp.tile_width * tp.tile_height;
        let k = idx % per;
        Some((self.c.trickplay_tile(&it.id, &ms.id, tp.width, idx / per), k % tp.tile_width, k / tp.tile_width, tp.width, tp.height))
    }

    fn drop_img(&mut self) {
        if let Some((_, _, _, rects)) = self.img_cur.take() { for (t, _) in rects { if let Some(t) = t { self.garbage.push(t); } } }
    }

    fn toggle_pause(&mut self, player: &mut Player) {
        let p = !player.paused();
        player.set_paused(p);
        self.flash = Some((if p { Icon::Pause } else { Icon::Play }, Instant::now()));
        if p { self.show_osd(3600); } else { self.show_osd(3); }
        self.report(player, "progress", if p { "pause" } else { "unpause" });
    }

    pub fn update(&mut self, player: &mut Player, cx: &mut Cx) -> (bool, Action) {
        let mut busy = false;
        for m in self.mb.drain() {
            match m {
                Msg::Item(Ok(it)) => {
                    self.ms = self.spec.source.as_ref().and_then(|id| it.media_sources.iter().find(|m| &m.id == id)).or(it.media_sources.first()).cloned();
                    // an episode started without a queue (a launcher card, Continue watching, search) or the last one
                    // of a season: the following episodes of the series, across seasons, so up next / autoplay go on
                    if self.spec.queue.is_empty() && it.is_episode() {
                        if let Some(series) = it.series_id.clone() {
                            let (c, tx, id) = (self.c.clone(), self.mb.tx.clone(), it.id.clone());
                            self.pool.spawn(move || {
                                let mut v = c.episodes_from(&series, &id).unwrap_or_default();
                                if v.first().map_or(false, |e| e.id == id) { v.remove(0); }
                                let _ = tx.send(Msg::Queue(v)); crate::net::wake();
                            });
                        }
                    }
                    self.item = Some(it);
                    if self.ms.is_none() { self.error = Some("This item has no playable file.".into()); continue; }
                    self.choose_tracks();
                    let s = self.start_ns();
                    self.last_pos = s;
                    self.launch(player, s);
                }
                Msg::Item(Err(e)) => self.error = Some(format!("Couldn't load the item: {e}")),
                Msg::Segments(v) => { crate::log!("segments: {:?}", v.iter().map(|s| (&s.kind, s.start_ticks / TICKS_PER_SEC, s.end_ticks / TICKS_PER_SEC)).collect::<Vec<_>>()); self.segments = v; }
                Msg::Srt(i, Ok(text)) => { if Some(i) == self.sub { self.cues = parse_srt(&text); crate::log!("subtitles: {} cues", self.cues.len()); } }
                Msg::Srt(_, Err(e)) => crate::log!("subtitles: {e}"),
                Msg::Queue(v) => { crate::log!("queue: {} more episodes", v.len()); if self.spec.queue.is_empty() { self.spec.queue = v; } }
            }
            busy = true;
        }
        if self.done { return (true, Action::None); }
        let phase = player.phase();
        let pos = player.position_ns();
        if player.has_picture() { self.last_pos = pos; }
        if self.pause_on_picture && player.has_picture() { self.pause_on_picture = false; if !player.paused() { self.toggle_pause(player); } }
        // first picture: tell the server we're playing
        if player.has_picture() && !self.reported_start {
            self.reported_start = true;
            crate::log!("first picture {} ms after pressing play", self.started_at.elapsed().as_millis());
            self.report(player, "start", "timeupdate");
        }
        if self.reported_start && self.last_report.elapsed() > Duration::from_secs(10) && phase == Phase::Playing { self.report(player, "progress", "timeupdate"); }
        // failure: once, fall back to having the server convert the whole stream
        if phase == Phase::Failed {
            let e = player.error().unwrap_or_default();
            if !self.fell_back && !player.has_picture() && self.method == Method::Direct {
                crate::log!("direct play failed ({e}), retrying with server conversion");
                self.fell_back = true;
                let p = self.last_pos;
                self.launch(player, p);
            } else if self.error.is_none() { self.error = Some(e); }
        }
        // end of the file
        if phase == Phase::Ended && !self.ended_handled {
            self.ended_handled = true;
            self.last_pos = self.duration_ns(player);
            if prefs().autoplay && !self.upnext_dismissed { if let Some(n) = self.next_spec() { self.exit(player); return (true, Action::Play(n)); } }
            self.exit(player);
            return (true, Action::None);
        }
        // Bluetooth headphones connected / disconnected mid-film, or a lip-sync change: restart the pipeline at the
        // current position with the new lead (an in-place seek, ~0.6 s)
        if self.bt_checked.elapsed() > Duration::from_secs(3) {
            self.bt_checked = Instant::now();
            let bt = crate::player::bluetooth_audio();
            if bt != self.bt { crate::log!("player: Bluetooth sound {}", if bt { "on" } else { "off" }); self.bt = bt; self.lead_pending = Some(Instant::now() - Duration::from_secs(1)); }
        }
        if self.lead_pending.map_or(false, |t| t.elapsed() > Duration::from_millis(600)) && self.seek.is_none() {
            self.lead_pending = None;
            if player.has_picture() && player.audio_lead_ns() != self.lead_ns() {
                let pos = player.position_ns();
                let paused = player.paused();
                crate::log!("player: lip sync now {} ms", self.lead_ns() / 1_000_000);
                self.launch(player, pos);
                if paused { self.pause_on_picture = true; }
            }
        }
        // seek commits after a short pause in the button presses
        if let Some((t, at, _)) = self.seek { if at.elapsed() > Duration::from_millis(700) { self.seek = None; self.seek_to(player, t); } busy = true; }
        // up next: at the credits segment, else 25 s before the end
        let dur = self.duration_ns(player);
        if !self.spec.queue.is_empty() && dur > 0 && self.upnext_at.is_none() && !self.upnext_dismissed && player.has_picture() {
            let outro = self.segments.iter().find(|s| s.kind == "Outro").map(|s| s.start_ticks * 100);
            let at = outro.unwrap_or(dur - 25_000_000_000);
            if pos >= at && pos < dur { self.upnext_at = Some(Instant::now()); }
        }
        if let Some(at) = self.upnext_at {
            if prefs().autoplay && at.elapsed() > Duration::from_secs(10) && !self.upnext_dismissed {
                if let Some(n) = self.next_spec() { self.exit(player); return (true, Action::Play(n)); }
            }
            busy = true;
        }
        // embedded text subtitles demuxed by the engine
        let texts = player.take_texts();
        if !texts.is_empty() {
            let mut touched = false;
            if let Some(ms) = self.ms.clone() {
                for ev in texts {
                    let Some(jf) = jf_index(&ms, ev.stream) else { continue };
                    let lines = if ev.ass { ass_lines(&ev.text) } else { ev.text.replace("\r\n", "\n").lines().map(clean_line).filter(|l| !l.is_empty()).collect() };
                    if lines.is_empty() { continue; }
                    let v = self.local_cues.entry(jf).or_default();
                    let at = v.partition_point(|c| c.start < ev.start_ns);
                    // ASS repeats a line on several layers; seeking back delivers events again
                    if v[at..].iter().take_while(|c| c.start == ev.start_ns).any(|c| c.lines == lines) { continue; }
                    v.insert(at, Cue { start: ev.start_ns, end: ev.end_ns, lines });
                    touched |= Some(jf) == self.sub;
                }
            }
            if touched { if let Some(i) = self.sub { if self.sub_is_local(i) { self.cues = self.local_cues.get(&i).cloned().unwrap_or_default(); } } }
        }
        // image subtitles: move due events to the screen
        let spos = self.sub_clock(player);
        for ev in player.take_subs() { self.img_q.push_back(ev); }
        while self.img_q.front().map_or(false, |e| e.start_ns <= spos) {
            let ev = self.img_q.pop_front().unwrap();
            self.drop_img();
            if !ev.rects.is_empty() {
                let rects = ev.rects.into_iter().map(|r| (None, r)).collect();
                self.img_cur = Some((ev.start_ns, ev.end_ns, ev.canvas, rects));
            }
            busy = true;
        }
        if let Some((_, Some(end), _, _)) = &self.img_cur { if spos > *end { self.drop_img(); } }
        // text subtitles: redraw exactly when the visible cue changes (rendering is event-driven)
        // (several cues can be up at once: the set is identified by its first index and count)
        let mut act = self.cues.iter().enumerate().take_while(|(_, c)| c.start <= spos).filter(|(_, c)| spos <= c.end).map(|(i, _)| i);
        let cue = act.next().map(|first| first * 1000 + 1 + act.count());
        if cue != self.shown_cue {
            self.shown_cue = cue;
            busy = true;
            if let Some(c) = cue.and_then(|c| self.cues.get(c / 1000)) { crate::log!("sub @{} ms: {}", spos / 1_000_000, c.lines.join(" / ").chars().take(80).collect::<String>()); }
        }
        // OSD auto-hide
        if Instant::now() > self.osd_until && self.seek.is_none() && !player.paused() { self.osd.set(0.0); }

        if !player.has_picture() || self.error.is_some() { self.osd.set(1.0); }
        busy |= self.osd.step(cx.dt);
        self.menu_a.set(if self.menu.is_some() { 1.0 } else { 0.0 });
        busy |= self.menu_a.step(cx.dt);
        if let Some((_, at)) = self.flash { if at.elapsed() > Duration::from_millis(700) { self.flash = None; } busy = true; }
        busy |= !player.has_picture();
        (busy, Action::None)
    }

    pub fn draw(&mut self, g: &mut Gfx, player: &Player, cx: &mut Cx) {
        let has_pic = player.has_picture();
        let t = cx.t;
        let o = smooth(self.osd.v);
        let it = self.item.clone().unwrap_or_else(|| self.spec.item.clone());
        // before the first picture: the item's backdrop, dimmed, under a spinner
        if !has_pic {
            if !player.active() || matches!(player.phase(), Phase::Opening | Phase::Loading) {
                g.rect(Rect::new(0.0, 0.0, 1920.0, 1080.0), BG, 0.0);
                let bd = self.c.backdrop(&it, 1920);
                if let Some((tex, a)) = cx.images.get(&bd) {
                    let tex = unsafe { &*(tex as *const Texture) };
                    g.image(tex, Rect::new(0.0, 0.0, 1920.0, 1080.0), 0.0, a * 0.35, 0.5, 0.3);
                }
            }
            if self.error.is_none() { spinner(g, 960.0, 540.0, 26.0, t, TEXT.a(0.9)); }
        }
        // subtitles (above the OSD when it is up)
        let lift = o * 250.0;
        for t in self.garbage.drain(..) { unsafe { g.free(t); } }
        if let Some((_, _, canvas, rects)) = self.img_cur.as_mut() {
            let (cw, ch) = (canvas.0 as f32, canvas.1 as f32);
            let s = (1920.0 / cw).min(1080.0 / ch);
            let (ox, oy) = ((1920.0 - cw * s) / 2.0, (1080.0 - ch * s) / 2.0);
            for (tex, r) in rects.iter_mut() {
                if tex.is_none() { *tex = Some(unsafe { g.upload_rgba(r.w as u32, r.h as u32, &r.rgba) }); }
                let t = tex.as_ref().unwrap();
                let t = unsafe { &*(t as *const Texture) };
                g.image(t, Rect::new(ox + r.x as f32 * s, oy + r.y as f32 * s - lift * 0.6, r.w as f32 * s, r.h as f32 * s), 0.0, 1.0, 0.5, 0.5);
            }
        }
        let pos = if has_pic { self.sub_clock(player) } else { self.last_pos };
        let mut shown: Vec<&String> = vec![];
        // not before the picture: after a seek playback starts at the keyframe before the target
        for c in self.cues.iter().filter(|_| has_pic).take_while(|c| c.start <= pos).filter(|c| pos <= c.end) {
            for l in &c.lines { if !shown.contains(&l) { shown.push(l); } }
        }
        if !shown.is_empty() {
            let size = [40.0, 50.0, 62.0][prefs().sub_size.min(2)];
            let mut lines = vec![];
            for l in shown.iter().rev().take(4).rev() { lines.extend(g.text.wrap(l, size, W::Semi, 1560.0, 2)); }
            let lh = size * 1.28;
            let mut y = 1080.0 - 92.0 - lift - lines.len() as f32 * lh;
            for l in lines {
                let w = g.text.measure(&l, size, W::Semi, 0.0);
                let x = 960.0 - w / 2.0;
                for (dx, dy) in [(-2.5f32, 0.0f32), (2.5, 0.0), (0.0, -2.5), (0.0, 2.5), (-1.8, -1.8), (1.8, -1.8), (-1.8, 1.8), (1.8, 1.8), (0.0, 4.0)] {
                    g.text(x + dx, y + dy, size, W::Semi, Color(0.0, 0.0, 0.0, 0.85), &l);
                }
                g.text(x, y, size, W::Semi, Color(1.0, 1.0, 1.0, 1.0), &l);
                y += lh;
            }
        }

        // OSD
        if o > 0.01 {
            g.alpha = o;
            g.vgrad(Rect::new(0.0, 0.0, 1920.0, 240.0), Color(0.0, 0.0, 0.0, 0.62), Color(0.0, 0.0, 0.0, 0.0), 0.0);
            g.vgrad(Rect::new(0.0, 600.0, 1920.0, 480.0), Color(0.0, 0.0, 0.0, 0.0), Color(0.0, 0.0, 0.0, 0.88), 0.0);
            g.text_r(1920.0 - PAD, 52.0, 40.0, W::Medium, TEXT, &hhmm(0));
            if prefs().stats {
                let m = match self.method { Method::Direct => "Direct play", Method::Convert => "Server converts the stream" };
                g.text(PAD, 56.0, 20.0, W::Medium, MUTED, &format!("{m}  ·  {}", player.info()));
            }
            // title block
            let title = it.title();
            let sub = if it.is_episode() { it.subtitle() } else { [it.production_year.map(|y| y.to_string()), it.official_rating.clone()].into_iter().flatten().collect::<Vec<_>>().join("  ·  ") };
            let tl = g.text.ellipsize(&title, 48.0, W::Bold, 1300.0);
            g.text_t(PAD, 772.0, 48.0, W::Bold, TEXT, &tl, -0.8);
            if !sub.is_empty() { let s = g.text.ellipsize(&sub, 26.0, W::Medium, 1300.0); g.text(PAD, 838.0, 26.0, W::Medium, MUTED, &s); }
            if player.paused() { pill(g, PAD, 714.0, "Paused", ACCENT); }
            // progress bar
            let dur = self.duration_ns(player).max(1);
            let bar = Rect::new(PAD, 912.0, 1920.0 - 2.0 * PAD, 6.0);
            g.rect(bar, Color(1.0, 1.0, 1.0, 0.22), 3.0);
            let shown_pos = pos.clamp(0, dur);
            let fx = bar.x + bar.w * shown_pos as f32 / dur as f32;
            g.rect(Rect::new(bar.x, bar.y, fx - bar.x, bar.h), ACCENT, 3.0);
            cx.hot.push((Rect::new(bar.x - 10.0, bar.y - 24.0, bar.w + 20.0, 54.0), Hot { row: 20, col: 0 }));
            for (t, _) in self.chapters().iter().skip(1) {
                let cxp = bar.x + bar.w * (*t as f32 / dur as f32);
                g.rect(Rect::new(cxp - 1.5, bar.y - 4.0, 3.0, bar.h + 8.0), Color(1.0, 1.0, 1.0, 0.6), 1.5);
            }
            // intro/credits segments as faint marks
            for s in &self.segments {
                let (a, b) = (s.start_ticks as f32 * 100.0 / dur as f32, s.end_ticks as f32 * 100.0 / dur as f32);
                g.rect(Rect::new(bar.x + bar.w * a, bar.y + 10.0, (bar.w * (b - a)).max(4.0), 3.0), Color(1.0, 1.0, 1.0, 0.25), 1.5);
            }
            let knob = if let Some((target, _, _)) = self.seek {
                let tx = bar.x + bar.w * target.clamp(0, dur) as f32 / dur as f32;
                g.rect(Rect::new(fx.min(tx), bar.y, (tx - fx).abs(), bar.h), Color(1.0, 1.0, 1.0, 0.55), 3.0);
                let lbl = fmt_time(target);
                let delta = (target - pos) / 1_000_000_000;
                let d = format!("{}{}", if delta >= 0 { "+" } else { "−" }, fmt_time(delta.abs() * 1_000_000_000));
                let w = g.text.measure(&lbl, 30.0, W::Semi, 0.0).max(g.text.measure(&d, 20.0, W::Medium, 0.0)) + 36.0;
                let bx = (tx - w / 2.0).clamp(PAD, 1920.0 - PAD - w);
                g.rect(Rect::new(bx, bar.y - 104.0, w, 86.0), Color(0.08, 0.08, 0.11, 0.94), 16.0);
                g.text_c(bx + w / 2.0, bar.y - 96.0, 30.0, W::Semi, TEXT, &lbl);
                g.text_c(bx + w / 2.0, bar.y - 56.0, 20.0, W::Medium, ACCENT, &d);
                if let Some((url, col, row, tw, th)) = self.trickplay_at(target) {
                    if let Some((tex, a)) = cx.images.get(&url) {
                        let tex = unsafe { &*(tex as *const Texture) };
                        let (pw, ph) = (384.0, 384.0 * th as f32 / tw.max(1) as f32);
                        let px = (tx - pw / 2.0).clamp(PAD, 1920.0 - PAD - pw);
                        let r = Rect::new(px, bar.y - 118.0 - ph, pw, ph);
                        g.rect(r.inset(-4.0), Color(0.08, 0.08, 0.11, 0.94), 14.0);
                        let (u0, v0) = ((col * tw) as f32 / tex.w as f32, (row * th) as f32 / tex.h as f32);
                        g.image_uv(tex, r, [u0, v0, u0 + tw as f32 / tex.w as f32, v0 + th as f32 / tex.h as f32], 10.0, a);
                    }
                }
                tx
            } else { fx };
            g.shadow(Rect::new(knob - 11.0, bar.y - 8.0, 22.0, 22.0), Color(0.0, 0.0, 0.0, 0.5), 11.0, 8.0);
            g.rect(Rect::new(knob - 10.0, bar.y - 7.0, 20.0, 20.0), Color(1.0, 1.0, 1.0, 1.0), 10.0);
            g.text(PAD, 940.0, 22.0, W::Medium, TEXT, &fmt_time(shown_pos));
            let left = (dur - shown_pos).max(0);
            g.text_r(1920.0 - PAD, 940.0, 22.0, W::Medium, MUTED, &format!("−{}  ·  ends {}", fmt_time(left), hhmm(left / 1_000_000_000)));
            // controls: the remote's keys in the labels, clickable with the pointer
            let mut ctl = vec![(Icon::Back10, "◀  10 s"), (if player.paused() { Icon::Play } else { Icon::Pause }, if player.paused() { "OK  Play" } else { "OK  Pause" }),
                (Icon::Fwd10, "▶  10 s"), (Icon::Subs, "▲  Audio, subtitles")];
            if self.chapters().len() > 1 { ctl.push((Icon::Next, "CH  Chapter")); }
            let mut hx = PAD - 14.0;
            for (i, (ic, h)) in ctl.into_iter().enumerate() {
                let w = 32.0 + g.text.measure(h, 19.0, W::Medium, 0.0) + 28.0;
                let r = Rect::new(hx, 986.0, w, 48.0);
                let hov = cx_hover(cx, r);
                if hov { g.rect(r, Color(1.0, 1.0, 1.0, 0.16), 24.0); }
                let ink = if hov { TEXT } else { FAINT };
                draw_icon(g, ic, hx + 14.0, 1000.0, 22.0, ink);
                g.text(hx + 46.0, 1000.0, 19.0, W::Medium, ink, h);
                cx.hot.push((r, Hot { row: 21, col: i as i32 }));
                hx += w + 12.0;
            }
            g.alpha = 1.0;
        }
        // centre flash on pause/play
        if let Some((ic, at)) = self.flash {
            let k = at.elapsed().as_secs_f32() / 0.7;
            let a = (1.0 - k).max(0.0);
            let s = 1.0 + 0.3 * k;
            let r = Rect::new(960.0 - 70.0 * s, 540.0 - 70.0 * s, 140.0 * s, 140.0 * s);
            g.rect(r, Color(0.0, 0.0, 0.0, 0.45 * a), 70.0 * s);
            draw_icon(g, ic, r.x + 35.0 * s, r.y + 35.0 * s, 70.0 * s, TEXT.a(a));
        }
        // skip intro
        if let Some(seg) = self.in_skippable().cloned() {
            let label = match seg.kind.as_str() { "Recap" => "Skip recap", "Preview" => "Skip preview", _ => "Skip intro" };
            let w = g.text.measure(label, 26.0, W::Semi, 0.0) + 80.0;
            let r = Rect::new(1920.0 - PAD - w, if o > 0.5 { 700.0 } else { 900.0 }, w, 70.0);
            let f = if o < 0.5 { 1.0 } else { 0.4 };
            let br = button(g, r.x, r.y, label, Some(Icon::Next), f, false, 70.0);
            cx.hot.push((br, Hot { row: 22, col: 0 }));
        }
        // up next
        if let (Some(at), Some(next)) = (self.upnext_at, self.spec.queue.first().cloned()) {
            if !self.upnext_dismissed {
                let a = smooth(at.elapsed().as_secs_f32() / 0.4);
                let r = Rect::new(1920.0 - PAD - 620.0 + (1.0 - a) * 40.0, if o > 0.5 { 560.0 } else { 790.0 }, 620.0, 190.0);
                g.alpha = a;
                cx.hot.push((r, Hot { row: 23, col: 0 }));
                g.shadow(r.offset(0.0, 16.0), Color(0.0, 0.0, 0.0, 0.6), 24.0, 40.0);
                g.rect(r, Color(0.06, 0.06, 0.09, 0.95), 24.0);
                let th = Rect::new(r.x + 18.0, r.y + 18.0, 274.0, 154.0);
                picture(g, cx, &self.c.thumb(&next, 560), th, 14.0, "");
                label(g, r.x + 316.0, r.y + 26.0, "Up next");
                let ttl = g.text.ellipsize(&format!("{} · {}", next.episode_code(), next.name), 24.0, W::Semi, 280.0);
                g.text(r.x + 316.0, r.y + 60.0, 24.0, W::Semi, TEXT, &ttl);
                if prefs().autoplay {
                    let left = 10.0 - at.elapsed().as_secs_f32();
                    g.text(r.x + 316.0, r.y + 98.0, 20.0, W::Regular, MUTED, &format!("Starts in {}", left.ceil().max(0.0)));
                    let pr = Rect::new(r.x + 316.0, r.y + 140.0, 270.0, 6.0);
                    g.rect(pr, Color(1.0, 1.0, 1.0, 0.18), 3.0);
                    g.rect(Rect::new(pr.x, pr.y, pr.w * (1.0 - left / 10.0).clamp(0.0, 1.0), pr.h), ACCENT, 3.0);
                } else {
                    g.text(r.x + 316.0, r.y + 98.0, 20.0, W::Regular, MUTED, "OK to play  ·  Back to dismiss");
                }
                g.alpha = 1.0;
            }
        }
        // track menu
        let ma = smooth(self.menu_a.v);
        if ma > 0.01 {
            let entries = self.entries();
            let w = 640.0;
            let x = 1920.0 - w * ma;
            g.alpha = ma;
            g.hgrad(Rect::new(x - 200.0, 0.0, 200.0, 1080.0), Color(0.0, 0.0, 0.0, 0.0), Color(0.03, 0.03, 0.05, 0.85), 0.0);
            g.rect(Rect::new(x, 0.0, w, 1080.0), Color(0.03, 0.03, 0.05, 0.9), 0.0);
            let idx = self.menu.as_ref().map_or(0, |m| m.idx);
            let start = idx.saturating_sub(9);
            let mut y = 90.0;
            for (i, (text, key)) in entries.iter().enumerate().skip(start) {
                if y > 1020.0 { break; }
                match key {
                    None => { y += 18.0; label(g, x + 56.0, y, text); y += 46.0; }
                    Some((kind, v)) => {
                        let sel = match kind { 'a' => self.audio == Some(*v), 'c' | 'd' => false, _ => if *v < 0 { self.sub.is_none() } else { self.sub == Some(*v) } };
                        let f = i == idx;
                        let r = Rect::new(x + 36.0, y, w - 72.0, 60.0);
                        if f { g.rect(r, Color(1.0, 1.0, 1.0, 1.0), 16.0); }
                        let ink = if f { BG } else { TEXT };
                        if sel { draw_icon(g, Icon::Check, r.x + 18.0, r.y + 17.0, 26.0, if f { BG } else { ACCENT }); }
                        let t = g.text.ellipsize(text, 23.0, W::Medium, r.w - 90.0);
                        g.text(r.x + 60.0, r.y + 16.0, 23.0, W::Medium, ink, &t);
                        cx.hot.push((r, Hot { row: 9, col: i as i32 }));
                        y += 66.0;
                    }
                }
            }
            g.alpha = 1.0;
        }
        // error
        if let Some(e) = &self.error {
            let r = Rect::new(560.0, 380.0, 800.0, 300.0);
            g.rect(r, Color(0.06, 0.06, 0.09, 0.96), 28.0);
            g.text_c(960.0, r.y + 50.0, 36.0, W::Bold, TEXT, "Can't play this");
            for (i, l) in g.text.wrap(e, 22.0, W::Regular, 700.0, 3).iter().enumerate() { g.text_c(960.0, r.y + 120.0 + i as f32 * 32.0, 22.0, W::Regular, MUTED, l); }
            g.text_c(960.0, r.bottom() - 60.0, 20.0, W::Medium, FAINT, "OK  Try again   ·   Back  Return");
            cx.hot.push((r, Hot { row: 24, col: 0 }));
        }

    }
}

/// The track menu: section headers (None) and selectable entries (kind 'a' audio / 's' subtitles, Jellyfin index; -1 = off).
fn menu_entries(ms: Option<&MediaSource>) -> Vec<(String, Option<(char, i32)>)> {
    let mut v = vec![("Audio".to_string(), None)];
    let Some(ms) = ms else { return v };
    for s in ms.media_streams.iter().filter(|s| s.kind == "Audio") {
        v.push((stream_label(s), Some(('a', s.index))));
    }
    v.push(("Subtitles".into(), None));
    v.push(("Off".into(), Some(('s', -1))));
    for s in ms.media_streams.iter().filter(|s| s.kind == "Subtitle") {
        v.push((stream_label(s), Some(('s', s.index))));
    }
    v
}

fn session_id() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") { let _ = std::io::Read::read_exact(&mut f, &mut b); }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn parse_srt(s: &str) -> Vec<Cue> {
    let ts = |t: &str| -> Option<i64> {
        let t = t.trim().replace(',', ".");
        let mut parts = t.split(':');
        let (h, m, rest) = (parts.next()?.parse::<i64>().ok()?, parts.next()?.parse::<i64>().ok()?, parts.next()?);
        let secs: f64 = rest.split_whitespace().next()?.parse().ok()?;
        Some(((h * 3600 + m * 60) as f64 * 1e9 + secs * 1e9) as i64)
    };
    let clean = clean_line;
    let mut cues = vec![];
    for block in s.replace("\r\n", "\n").split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some(ti) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let mut it = lines[ti].split("-->");
        let (Some(a), Some(b)) = (it.next().and_then(ts), it.next().and_then(ts)) else { continue };
        let text: Vec<String> = lines[ti + 1..].iter().map(|l| clean(l)).filter(|l| !l.is_empty()).collect();
        if !text.is_empty() { cues.push(Cue { start: a, end: b, lines: text }); }
    }
    cues.sort_by_key(|c| c.start);
    cues
}

/// One subtitle line without HTML-ish and ASS override tags.
fn clean_line(l: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    for ch in l.chars() {
        match ch { '<' | '{' => depth += 1, '>' | '}' => depth -= 1, c if depth <= 0 => out.push(c), _ => {} }
        if depth < 0 { depth = 0; }
    }
    out.replace("\\N", " ").replace("\\h", " ").replace("&amp;", "&").trim().to_string()
}

/// A Matroska ASS event ("ReadOrder,Layer,Style,Name,MarginL,MarginR,MarginV,Effect,Text") -> display lines.
/// Override blocks are dropped; drawings (\p1 ...) are vector shapes, not text, and are skipped entirely.
fn ass_lines(ev: &str) -> Vec<String> {
    let Some(text) = ev.splitn(9, ',').nth(8) else { return vec![] };
    let mut out = String::new();
    let mut block: Option<String> = None;
    for ch in text.chars() {
        match (&mut block, ch) {
            (None, '{') => block = Some(String::new()),
            (Some(b), '}') => {
                let drawing = b.match_indices("\\p").any(|(i, _)| b[i + 2..].chars().next().map_or(false, |c| ('1'..='9').contains(&c)));
                if drawing { return vec![]; }
                block = None;
            }
            (Some(b), c) => b.push(c),
            (None, c) => out.push(c),
        }
    }
    out.replace("\\N", "\n").replace("\\n", "\n").replace("\\h", " ")
        .lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()
}

/// Is the pointer over `r`? (for hover highlights on the player's controls)
fn cx_hover(_cx: &Cx, r: Rect) -> bool { pointer_at().map_or(false, |(x, y)| r.contains(x, y)) }
