//! Jellyfin API client (blocking; call from the pool) + the models the UI uses.
//! Sign-in is Quick Connect only; the session lives in /media/developer/jfnative/session.json (outside
//! the app folder, so reinstalling the app keeps you signed in).
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// A server to suggest on the sign-in screen, baked in at build time (`JFNATIVE_SERVER=https://… cargo build`); none by default.
pub const BUILD_SERVER: Option<&str> = option_env!("JFNATIVE_SERVER");
pub const DATA_DIR: &str = "/media/developer/jfnative";
pub const TICKS_PER_SEC: i64 = 10_000_000;

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct Session { pub server: String, pub token: String, pub user_id: String, pub user_name: String, pub device_id: String }

impl Session {
    pub fn load() -> Option<Session> {
        let s: Session = serde_json::from_str(&std::fs::read_to_string(format!("{DATA_DIR}/session.json")).ok()?).ok()?;
        (!s.token.is_empty()).then_some(s)
    }
    pub fn save(&self) {
        let _ = std::fs::create_dir_all(DATA_DIR);
        let tmp = format!("{DATA_DIR}/session.json.tmp");
        // the access token: readable by this app's user only
        if write_private(&tmp, serde_json::to_string(self).unwrap().as_bytes()).is_ok() { let _ = std::fs::rename(&tmp, format!("{DATA_DIR}/session.json")); }
        let _ = std::fs::write(format!("{DATA_DIR}/last-server"), &self.server);
    }
    pub fn clear() { let _ = std::fs::remove_file(format!("{DATA_DIR}/session.json")); }
}

fn write_private(path: &str, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::remove_file(path);
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?.write_all(data)
}

/// At start: the data folder and the session file (access token) belong to this app only.
pub fn secure_data_dir() {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::create_dir_all(DATA_DIR);
    let _ = std::fs::set_permissions(DATA_DIR, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::set_permissions(format!("{DATA_DIR}/session.json"), std::fs::Permissions::from_mode(0o600));
}

/// The server used last (pre-selected on the sign-in screen after a sign-out).
pub fn last_server() -> Option<String> { std::fs::read_to_string(format!("{DATA_DIR}/last-server")).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) }

/// A server found on the network or checked by address.
#[derive(Clone, Debug)]
pub struct Found { pub url: String, pub name: String }

/// Jellyfin's LAN discovery: a UDP broadcast to port 7359, every server answers with its address and name.
pub fn discover() -> Vec<Found> {
    let Ok(sock) = std::net::UdpSocket::bind("0.0.0.0:0") else { return vec![] };
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(300)));
    let _ = sock.send_to(b"who is JellyfinServer?", "255.255.255.255:7359");
    let mut out: Vec<Found> = vec![];
    let until = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    let mut buf = [0u8; 2048];
    while std::time::Instant::now() < until {
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if let Ok(v) = serde_json::from_slice::<Value>(&buf[..n]) {
                let url = v["Address"].as_str().unwrap_or("").trim_end_matches('/').to_string();
                if !url.is_empty() && !out.iter().any(|f| f.url == url) { out.push(Found { url, name: v["Name"].as_str().unwrap_or("Jellyfin").into() }); }
            }
        }
    }
    crate::log!("discovery: {} server(s)", out.len());
    out
}

/// What the user typed -> a working server URL: tries it as given, then https, http and Jellyfin's port 8096.
pub fn check_server(agent: &ureq::Agent, input: &str) -> Result<Found, String> {
    let t = input.trim().trim_end_matches('/');
    if t.is_empty() { return Err("Enter your server's address first.".into()); }
    let mut tries = vec![];
    if t.starts_with("http://") || t.starts_with("https://") { tries.push(t.to_string()); }
    else {
        tries.push(format!("https://{t}"));
        tries.push(format!("http://{t}"));
        if !t.contains(':') { tries.push(format!("http://{t}:8096")); }
    }
    let mut last = String::new();
    for u in tries {
        match agent.get(&format!("{u}/System/Info/Public")).timeout(std::time::Duration::from_secs(5)).call() {
            Ok(r) => match r.into_json::<Value>() {
                Ok(v) if v["Id"].is_string() => return Ok(Found { url: u, name: v["ServerName"].as_str().unwrap_or("Jellyfin").into() }),
                _ => last = format!("{u} answered, but it isn't a Jellyfin server"),
            },
            Err(e) => last = err(e),
        }
    }
    Err(last)
}

pub fn device_id() -> String {
    let p = format!("{DATA_DIR}/device-id");
    if let Ok(s) = std::fs::read_to_string(&p) { if s.trim().len() > 8 { return s.trim().to_string(); } }
    let mut b = [0u8; 12];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") { let _ = std::io::Read::read_exact(&mut f, &mut b); }
    let id = format!("jfnative-{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>());
    let _ = std::fs::create_dir_all(DATA_DIR);
    let _ = std::fs::write(&p, &id);
    id
}

#[derive(Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct UserData {
    pub played_percentage: Option<f64>,
    pub playback_position_ticks: i64,
    pub played: bool,
    pub is_favorite: bool,
    pub unplayed_item_count: Option<i32>,
    pub last_played_date: Option<String>,
}

#[derive(Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaStream {
    #[serde(rename = "Type")] pub kind: String,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub display_title: Option<String>,
    pub index: i32,
    pub is_default: bool,
    pub is_forced: bool,
    pub is_external: bool,
    pub channels: Option<i32>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub real_frame_rate: Option<f32>,
    pub video_range_type: Option<String>,
    pub profile: Option<String>,
    pub is_text_subtitle_stream: bool,
    pub delivery_url: Option<String>,
}

#[derive(Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct MediaSource {
    pub id: String,
    pub name: Option<String>,
    pub container: Option<String>,
    pub size: Option<i64>,
    pub bitrate: Option<i64>,
    pub run_time_ticks: Option<i64>,
    pub media_streams: Vec<MediaStream>,
    pub default_audio_stream_index: Option<i32>,
    pub default_subtitle_stream_index: Option<i32>,
}

#[derive(Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct Chapter { pub start_position_ticks: i64, pub name: Option<String> }

/// One trickplay resolution: `tile_width` x `tile_height` thumbnails per sprite image, one every `interval` ms.
#[derive(Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct Trickplay { pub width: i32, pub height: i32, pub tile_width: i32, pub tile_height: i32, pub thumbnail_count: i32, pub interval: i32 }

#[derive(Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct Person { pub id: String, pub name: String, pub role: Option<String>, #[serde(rename = "Type")] pub kind: String, pub primary_image_tag: Option<String> }

#[derive(Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "PascalCase", default)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub server_id: Option<String>,
    #[serde(rename = "Type")] pub kind: String,
    pub collection_type: Option<String>,
    pub series_name: Option<String>,
    pub series_id: Option<String>,
    pub season_id: Option<String>,
    pub season_name: Option<String>,
    pub parent_index_number: Option<i32>,
    pub index_number: Option<i32>,
    pub production_year: Option<i32>,
    pub premiere_date: Option<String>,
    pub official_rating: Option<String>,
    pub community_rating: Option<f32>,
    pub critic_rating: Option<f32>,
    pub run_time_ticks: Option<i64>,
    pub overview: Option<String>,
    pub taglines: Vec<String>,
    pub genres: Vec<String>,
    pub studios: Vec<Value>,
    pub status: Option<String>,
    pub child_count: Option<i32>,
    pub image_tags: HashMap<String, String>,
    pub backdrop_image_tags: Vec<String>,
    pub parent_backdrop_item_id: Option<String>,
    pub parent_backdrop_image_tags: Vec<String>,
    pub parent_thumb_item_id: Option<String>,
    pub parent_thumb_image_tag: Option<String>,
    pub parent_logo_item_id: Option<String>,
    pub parent_logo_image_tag: Option<String>,
    pub series_primary_image_tag: Option<String>,
    pub series_thumb_image_tag: Option<String>,
    pub primary_image_aspect_ratio: Option<f32>,
    pub user_data: Option<UserData>,
    pub media_sources: Vec<MediaSource>,
    pub media_streams: Vec<MediaStream>,
    pub chapters: Vec<Chapter>,
    /// media source id -> thumbnail width -> layout
    pub trickplay: HashMap<String, HashMap<String, Trickplay>>,
    pub people: Vec<Person>,
}

impl Item {
    pub fn is_episode(&self) -> bool { self.kind == "Episode" }
    pub fn is_series(&self) -> bool { self.kind == "Series" }
    pub fn is_folder_like(&self) -> bool { matches!(self.kind.as_str(), "Series" | "Season" | "BoxSet" | "CollectionFolder" | "Folder") }
    pub fn played(&self) -> bool { self.user_data.as_ref().map_or(false, |u| u.played) }
    pub fn favorite(&self) -> bool { self.user_data.as_ref().map_or(false, |u| u.is_favorite) }
    pub fn progress(&self) -> f32 { self.user_data.as_ref().and_then(|u| u.played_percentage).map(|p| (p / 100.0) as f32).unwrap_or(0.0).clamp(0.0, 1.0) }
    pub fn resume_ticks(&self) -> i64 { self.user_data.as_ref().map_or(0, |u| u.playback_position_ticks) }
    pub fn minutes(&self) -> i64 { self.run_time_ticks.unwrap_or(0) / TICKS_PER_SEC / 60 }
    /// "S2 · E5"
    pub fn episode_code(&self) -> String {
        match (self.parent_index_number, self.index_number) {
            (Some(s), Some(e)) => format!("S{s} · E{e}"),
            (None, Some(e)) => format!("E{e}"),
            _ => String::new(),
        }
    }
    /// The big title: the series for an episode, else the item's own name.
    pub fn title(&self) -> String { if self.is_episode() { self.series_name.clone().unwrap_or_else(|| self.name.clone()) } else { self.name.clone() } }
    pub fn subtitle(&self) -> String {
        if self.is_episode() {
            let c = self.episode_code();
            if c.is_empty() { self.name.clone() } else { format!("{c} — {}", self.name) }
        } else {
            [self.production_year.map(|y| y.to_string()), Some(match self.kind.as_str() { "Movie" => "Movie", "Series" => "Series", k => k }.to_string())]
                .into_iter().flatten().collect::<Vec<_>>().join(" · ")
        }
    }
    pub fn minutes_left(&self) -> i64 {
        let total = self.run_time_ticks.unwrap_or(0);
        ((total - self.resume_ticks()).max(0) / TICKS_PER_SEC + 59) / 60
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
struct ItemsResult { items: Vec<Item>, total_record_count: i64 }

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "PascalCase")]
pub struct Segment { #[serde(rename = "Type")] pub kind: String, pub start_ticks: i64, pub end_ticks: i64 }

#[derive(Clone)]
pub struct Client { pub s: Session, pub agent: ureq::Agent }

pub fn auth_header(device_id: &str, token: Option<&str>) -> String {
    let mut h = format!("MediaBrowser Client=\"Jellyfin Native\", Device=\"LG TV\", DeviceId=\"{device_id}\", Version=\"{}\"", crate::VERSION);
    if let Some(t) = token { h.push_str(&format!(", Token=\"{t}\"")); }
    h
}

/// Set when the server rejects our token (signed out elsewhere, token revoked): the app goes back to sign-in.
pub static AUTH_LOST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn err(e: ureq::Error) -> String {
    use ureq::ErrorKind as K;
    match e {
        ureq::Error::Status(401, _) => { AUTH_LOST.store(true, std::sync::atomic::Ordering::Release); "your sign-in is no longer valid".into() }
        ureq::Error::Status(c, _) if c >= 500 => format!("the server had a problem (error {c})"),
        ureq::Error::Status(c, r) => format!("{c} {}", r.status_text()),
        ureq::Error::Transport(t) => match t.kind() {
            K::Dns | K::ConnectionFailed => "the server can't be reached - is it running, and is the TV online?".into(),
            K::Io => "the connection to the server dropped".into(),
            k => format!("network problem ({k})"),
        },
    }
}

/// Errors that a retry might fix (network, server), as opposed to e.g. a missing item.
pub fn retryable(e: &str) -> bool { e.contains("reach") || e.contains("connection") || e.contains("network") || e.contains("server had") }

const FIELDS: &str = "Overview,Genres,Taglines,PrimaryImageAspectRatio,ProductionYear,ChildCount,Studios";
const IMG: &str = "enableImageTypes=Primary,Backdrop,Thumb,Logo&imageTypeLimit=1";

impl Client {
    pub fn auth(&self) -> String { auth_header(&self.s.device_id, Some(&self.s.token)) }

    pub fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let r = self.agent.get(&format!("{}{path}", self.s.server)).set("Authorization", &self.auth()).call().map_err(err)?;
        r.into_json::<T>().map_err(|e| format!("bad reply: {e}"))
    }
    pub fn post(&self, path: &str, body: Value) -> Result<(), String> {
        self.agent.post(&format!("{}{path}", self.s.server)).set("Authorization", &self.auth()).send_json(body).map_err(err)?;
        Ok(())
    }
    pub fn delete(&self, path: &str) -> Result<(), String> {
        self.agent.delete(&format!("{}{path}", self.s.server)).set("Authorization", &self.auth()).call().map_err(err)?;
        Ok(())
    }

    fn items(&self, path: &str) -> Result<Vec<Item>, String> { Ok(self.get::<ItemsResult>(path)?.items) }

    pub fn resume(&self) -> Result<Vec<Item>, String> {
        self.items(&format!("/UserItems/Resume?userId={}&limit=16&mediaTypes=Video&fields={FIELDS}&{IMG}", self.s.user_id))
    }
    pub fn next_up(&self) -> Result<Vec<Item>, String> {
        self.items(&format!("/Shows/NextUp?userId={}&limit=16&enableResumable=false&enableRewatching=false&fields={FIELDS}&{IMG}", self.s.user_id))
    }
    pub fn views(&self) -> Result<Vec<Item>, String> { self.items(&format!("/UserViews?userId={}", self.s.user_id)) }
    pub fn latest(&self, parent: &str) -> Result<Vec<Item>, String> {
        self.get::<Vec<Item>>(&format!("/Items/Latest?userId={}&parentId={parent}&limit=20&groupItems=true&fields={FIELDS}&{IMG}", self.s.user_id))
    }
    /// A page of items. `query` narrows it further: "parentId=…", "genres=…", "personIds=…", "filters=IsUnplayed".
    pub fn library(&self, query: &str, kinds: &str, sort: &str, start: usize, limit: usize) -> Result<(Vec<Item>, i64), String> {
        let order = if sort == "SortName" { "Ascending" } else { "Descending" };
        let r: ItemsResult = self.get(&format!(
            "/Items?userId={}&{query}&includeItemTypes={kinds}&recursive=true&sortBy={sort},SortName&sortOrder={order}&startIndex={start}&limit={limit}&fields={FIELDS}&{IMG}&enableTotalRecordCount=true",
            self.s.user_id))?;
        Ok((r.items, r.total_record_count))
    }
    pub fn item(&self, id: &str) -> Result<Item, String> {
        self.get(&format!("/Items/{id}?userId={}&fields={FIELDS},MediaSources,MediaStreams,People,Chapters,Trickplay", self.s.user_id))
    }
    pub fn seasons(&self, series: &str) -> Result<Vec<Item>, String> {
        self.items(&format!("/Shows/{series}/Seasons?userId={}&fields={FIELDS}&{IMG}", self.s.user_id))
    }
    pub fn episodes(&self, series: &str, season: &str) -> Result<Vec<Item>, String> {
        self.items(&format!("/Shows/{series}/Episodes?userId={}&seasonId={season}&fields={FIELDS},MediaSources&{IMG}", self.s.user_id))
    }
    /// The series' episodes from `start` on, across seasons (what plays next after it).
    pub fn episodes_from(&self, series: &str, start: &str) -> Result<Vec<Item>, String> {
        self.items(&format!("/Shows/{series}/Episodes?userId={}&startItemId={start}&isMissing=false&limit=40&fields={FIELDS}&{IMG}", self.s.user_id))
    }
    pub fn similar(&self, id: &str) -> Result<Vec<Item>, String> {
        self.items(&format!("/Items/{id}/Similar?userId={}&limit=16&fields={FIELDS}&{IMG}", self.s.user_id))
    }
    pub fn search(&self, term: &str) -> Result<Vec<Item>, String> {
        let t = enc(term);
        self.items(&format!("/Items?userId={}&searchTerm={t}&recursive=true&includeItemTypes=Movie,Series,Episode&limit=36&fields={FIELDS}&{IMG}", self.s.user_id))
    }
    pub fn segments(&self, id: &str) -> Vec<Segment> {
        #[derive(Deserialize, Default)] #[serde(rename_all = "PascalCase", default)] struct R { items: Vec<Segment> }
        self.get::<R>(&format!("/MediaSegments/{id}")).map(|r| r.items).unwrap_or_default()
    }
    pub fn set_played(&self, id: &str, played: bool) -> Result<(), String> {
        let p = format!("/UserPlayedItems/{id}?userId={}", self.s.user_id);
        if played { self.post(&p, json!({})) } else { self.delete(&p) }
    }
    pub fn set_favorite(&self, id: &str, fav: bool) -> Result<(), String> {
        let p = format!("/UserFavoriteItems/{id}?userId={}", self.s.user_id);
        if fav { self.post(&p, json!({})) } else { self.delete(&p) }
    }
    pub fn subtitle_srt(&self, item: &str, source: &str, index: i32) -> Result<String, String> {
        let r = self.agent.get(&format!("{}/Videos/{item}/{source}/Subtitles/{index}/0/Stream.srt", self.s.server))
            .set("Authorization", &self.auth()).call().map_err(err)?;
        r.into_string().map_err(|e| e.to_string())
    }

    // ---- playback reporting (keeps "continue watching" right everywhere, incl. Minimal Home)
    pub fn report(&self, what: &str, body: Value) {
        let path = match what { "start" => "/Sessions/Playing", "stop" => "/Sessions/Playing/Stopped", _ => "/Sessions/Playing/Progress" };
        if let Err(e) = self.post(path, body) { crate::log!("report {what}: {e}"); }
    }
    pub fn stop_transcode(&self, play_session: &str) {
        let _ = self.delete(&format!("/Videos/ActiveEncodings?deviceId={}&playSessionId={play_session}", self.s.device_id));
    }

    /// One trickplay sprite sheet (index = thumbnail / (tile_width * tile_height)).
    pub fn trickplay_tile(&self, item: &str, source: &str, width: i32, index: i32) -> String {
        format!("{}/Videos/{item}/Trickplay/{width}/{index}.jpg?MediaSourceId={source}", self.s.server)
    }
    pub fn person_img(&self, p: &Person, w: u32) -> String { self.img(&p.id, "Primary", p.primary_image_tag.as_deref(), w) }

    // ---- images
    pub fn img(&self, id: &str, kind: &str, tag: Option<&str>, w: u32) -> String {
        match tag { Some(t) => format!("{}/Items/{id}/Images/{kind}?maxWidth={w}&quality=82&tag={t}", self.s.server), None => String::new() }
    }
    /// Portrait poster (movies, series, seasons; the series poster for an episode).
    pub fn poster(&self, it: &Item, w: u32) -> String {
        if it.is_episode() {
            if let (Some(sid), Some(t)) = (&it.series_id, &it.series_primary_image_tag) { return self.img(sid, "Primary", Some(t), w); }
        }
        self.img(&it.id, "Primary", it.image_tags.get("Primary").map(|s| s.as_str()), w)
    }
    /// Landscape card image: the episode still / item thumb, then parent thumb, then a backdrop.
    pub fn thumb(&self, it: &Item, w: u32) -> String {
        if it.is_episode() { if let Some(t) = it.image_tags.get("Primary") { return self.img(&it.id, "Primary", Some(t), w); } }
        if let Some(t) = it.image_tags.get("Thumb") { return self.img(&it.id, "Thumb", Some(t), w); }
        if let (Some(p), Some(t)) = (&it.parent_thumb_item_id, &it.parent_thumb_image_tag) { return self.img(p, "Thumb", Some(t), w); }
        let b = self.backdrop(it, w);
        if !b.is_empty() { return b; }
        self.img(&it.id, "Primary", it.image_tags.get("Primary").map(|s| s.as_str()), w)
    }
    pub fn backdrop(&self, it: &Item, w: u32) -> String {
        if let Some(t) = it.backdrop_image_tags.first() { return self.img(&it.id, "Backdrop", Some(t), w); }
        if let (Some(p), Some(t)) = (&it.parent_backdrop_item_id, it.parent_backdrop_image_tags.first()) { return self.img(p, "Backdrop", Some(t), w); }
        String::new()
    }
    pub fn logo(&self, it: &Item, w: u32) -> String {
        if let Some(t) = it.image_tags.get("Logo") { return self.img(&it.id, "Logo", Some(t), w); }
        if let (Some(p), Some(t)) = (&it.parent_logo_item_id, &it.parent_logo_image_tag) { return self.img(p, "Logo", Some(t), w); }
        String::new()
    }
}

// ---- Quick Connect (no token yet)
pub struct QuickConnect { pub code: String, pub secret: String }

pub fn qc_initiate(agent: &ureq::Agent, server: &str, device: &str) -> Result<QuickConnect, String> {
    let h = auth_header(device, None);
    let en = agent.get(&format!("{server}/QuickConnect/Enabled")).set("Authorization", &h).call().map_err(err)?.into_string().unwrap_or_default();
    if en.trim() != "true" { return Err("Quick Connect is turned off on the server (Dashboard → General)".into()); }
    let v: Value = agent.post(&format!("{server}/QuickConnect/Initiate")).set("Authorization", &h).call().map_err(err)?.into_json().map_err(|e| e.to_string())?;
    Ok(QuickConnect { code: v["Code"].as_str().unwrap_or("").into(), secret: v["Secret"].as_str().unwrap_or("").into() })
}

/// Ok(Some) once approved, Ok(None) while pending.
pub fn qc_poll(agent: &ureq::Agent, server: &str, device: &str, secret: &str) -> Result<Option<Session>, String> {
    let h = auth_header(device, None);
    let v: Value = agent.get(&format!("{server}/QuickConnect/Connect?secret={secret}")).set("Authorization", &h).call().map_err(err)?.into_json().map_err(|e| e.to_string())?;
    if v["Authenticated"].as_bool() != Some(true) { return Ok(None); }
    let a: Value = agent.post(&format!("{server}/Users/AuthenticateWithQuickConnect")).set("Authorization", &h)
        .send_json(json!({"Secret": secret})).map_err(err)?.into_json().map_err(|e| e.to_string())?;
    Ok(Some(Session {
        server: server.to_string(),
        token: a["AccessToken"].as_str().unwrap_or("").into(),
        user_id: a["User"]["Id"].as_str().unwrap_or("").into(),
        user_name: a["User"]["Name"].as_str().unwrap_or("").into(),
        device_id: device.to_string(),
    }))
}

/// Percent-encoding for a query value.
pub fn enc(v: &str) -> String { v.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect() }

pub fn fmt_time(ns: i64) -> String {
    let s = (ns / 1_000_000_000).max(0);
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}
