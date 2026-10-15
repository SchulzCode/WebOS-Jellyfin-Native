//! Optional "Downloading" row: what Sonarr / Radarr are downloading right now, read from the small read-only feed service
//! of Minimal Home (WebOS-Minimal-Launcher, `server/launcher-feed`), so the TV never holds the *arr API keys. The feed is
//! looked for on the Jellyfin server's host, port 8091 (or `feed_url` in prefs.json); without one the row simply never
//! appears. Switch it off in Settings.
//! The same service reports the media disks' free space, and - if the server allows it (ALLOW_DELETE) - deletes a film
//! or a whole series through Sonarr/Radarr for a Jellyfin administrator (the app sends its own Jellyfin sign-in, which
//! the service checks with Jellyfin).
use serde::Deserialize;
use std::time::Duration;

pub const PORT: u16 = 8091;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Download {
    pub id: String,
    /// "episode" | "movie"
    pub kind: String,
    pub title: String,
    /// "S02E03", "S02 · 9 episodes", "" for films
    pub label: String,
    /// the episode's name when it is one episode
    pub name: String,
    /// 0..1, estimated by the feed for the moment it answered
    pub progress: f32,
    /// seconds left from the moment the feed answered (only while downloading)
    pub eta: Option<f32>,
    /// downloading, queued, paused, stalled, warning, importing, blocked, failed, done
    pub state: String,
    pub size: u64,
    pub year: Option<i32>,
    pub overview: String,
    pub fanart: String,
    pub thumb: String,
    pub logo: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Feed { downloading: Vec<Download> }

/// The service's address ("http://host:8091"): `feed_url` from prefs.json (without "/feed"), else the Jellyfin server's
/// host on port 8091. Used for the downloads, the storage panel and deleting.
pub fn base(server: &str) -> Option<String> {
    let p = crate::app::prefs();
    if !p.feed_url.is_empty() { let u = p.feed_url.trim_end_matches('/'); return Some(u.strip_suffix("/feed").unwrap_or(u).to_string()); }
    // the host of the Jellyfin address, without scheme, port and path ("[::1]" stays bracketed)
    let rest = server.split("://").nth(1).unwrap_or(server);
    let netloc = rest.split('/').next().unwrap_or("");
    let host = if netloc.starts_with('[') { netloc.split(']').next().map(|h| format!("{h}]")).unwrap_or_default() } else { netloc.split(':').next().unwrap_or("").to_string() };
    (!host.is_empty()).then(|| format!("http://{host}:{PORT}"))
}

/// Where to ask for the downloads, or None when the row is switched off in Settings.
pub fn url(server: &str) -> Option<String> {
    if !crate::app::prefs().downloads { return None; }
    base(server).map(|b| format!("{b}/feed"))
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Volume { pub path: String, pub free: u64, pub total: u64 }

/// The disks holding the media (usually one), and whether the server allows deleting (ALLOW_DELETE).
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Storage { pub volumes: Vec<Volume>, pub delete: bool }

pub fn storage(agent: &ureq::Agent, base: &str) -> Result<Storage, String> {
    let r = agent.get(&format!("{base}/storage")).timeout(Duration::from_secs(6)).call().map_err(|e| e.to_string())?;
    r.into_json::<Storage>().map_err(|e| e.to_string())
}

/// What a delete would remove (and, after a delete, what it removed).
#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Removal { pub app: String, pub title: String, pub year: Option<i32>, pub size: u64, pub files: u32, pub seasons: u32, pub volumes: Vec<Volume> }

#[derive(Deserialize, Default)]
#[serde(default)]
struct ErrorBody { error: String }

fn manage(agent: &ureq::Agent, base: &str, auth: &str, what: &str, id: &str) -> Result<Removal, String> {
    let url = format!("{base}/manage/{what}?id={id}");
    let req = if what == "delete" { agent.post(&url) } else { agent.get(&url) };
    match req.set("Authorization", auth).timeout(Duration::from_secs(if what == "delete" { 60 } else { 15 })).call() {
        Ok(r) => r.into_json::<Removal>().map_err(|e| e.to_string()),
        Err(ureq::Error::Status(code, r)) => Err(r.into_json::<ErrorBody>().ok().map(|b| b.error).filter(|e| !e.is_empty()).unwrap_or_else(|| format!("the server said {code}"))),
        Err(e) => Err(format!("can't reach the server's helper ({e})")),
    }
}

/// What deleting this film / series would remove, plus the current free space. Err explains why it can't be deleted.
pub fn preview(agent: &ureq::Agent, base: &str, auth: &str, id: &str) -> Result<Removal, String> { manage(agent, base, auth, "item", id) }

/// Delete the film / the whole series (files included) through Sonarr/Radarr.
pub fn delete(agent: &ureq::Agent, base: &str, auth: &str, id: &str) -> Result<Removal, String> { manage(agent, base, auth, "delete", id) }

/// The downloads worth showing, with picture addresses made absolute. Err = no feed there (or it failed).
pub fn fetch(agent: &ureq::Agent, url: &str) -> Result<Vec<Download>, String> {
    let resp = agent.get(url).timeout(Duration::from_secs(5)).call().map_err(|e| e.to_string())?;
    let feed: Feed = resp.into_json().map_err(|e| e.to_string())?;
    // "http://host:8091" + "/art/..."
    let base = url.find("://").and_then(|i| url[i + 3..].find('/').map(|j| &url[..i + 3 + j])).unwrap_or(url).to_string();
    let abs = |s: &str| if s.starts_with('/') { format!("{base}{s}") } else { s.to_string() };
    Ok(feed.downloading.into_iter().filter(|d| d.state != "done" && !d.state.is_empty()).map(|mut d| {
        d.fanart = abs(&d.fanart); d.thumb = abs(&d.thumb); d.logo = abs(&d.logo);
        d
    }).collect())
}

impl Download {
    /// Progress and seconds left `age` seconds after the feed answered: carried forward at the feed's rate, so the
    /// numbers keep moving between requests (at most 2 minutes, in case the download stalled meanwhile).
    pub fn live(&self, age: f32) -> (f32, Option<f32>) {
        let Some(eta) = self.eta else { return (self.progress, None) };
        let dt = age.clamp(0.0, 120.0);
        let left = (eta - dt).max(0.0);
        if self.state != "downloading" || eta <= 0.0 { return (self.progress, Some(left)); }
        ((self.progress + (1.0 - self.progress) * (dt / eta).min(1.0)).min(1.0), Some(left))
    }

    pub fn active(&self) -> bool { matches!(self.state.as_str(), "downloading" | "queued") }

    /// "7 % · 12 min left", "Queued", "Adding to your library", ...
    pub fn status_line(&self, age: f32) -> String {
        let (p, left) = self.live(age);
        let pct = format!("{} %", (p * 100.0).floor() as i32);
        match self.state.as_str() {
            "downloading" => match left { Some(s) if s > 0.0 => format!("{pct}  ·  {} left", duration(s)), _ => pct },
            "queued" => "Waiting to start".into(),
            "paused" => format!("{pct}  ·  paused"),
            "stalled" => format!("{pct}  ·  stalled, no sources right now"),
            "warning" => format!("{pct}  ·  needs a look in Sonarr/Radarr"),
            "importing" => "Adding to your library".into(),
            "blocked" => "Downloaded, but not imported - check Sonarr/Radarr".into(),
            "failed" => "Download failed".into(),
            s => s.to_string(),
        }
    }

    /// The short word for the hero pill.
    pub fn pill(&self) -> &'static str {
        match self.state.as_str() {
            "downloading" => "Downloading", "queued" => "Queued", "paused" => "Paused", "stalled" => "Stalled",
            "importing" => "Almost ready", "blocked" | "failed" | "warning" => "Problem", _ => "Download",
        }
    }
}

/// "under a minute", "7 min", "1 h 12 min", "2 d 3 h"
pub fn duration(s: f32) -> String {
    let m = (s / 60.0).round() as i64;
    if m < 1 { return "under a minute".into(); }
    if m < 60 { return format!("{m} min"); }
    if m < 48 * 60 { return format!("{} h {} min", m / 60, m % 60); }
    format!("{} d {} h", m / 1440, (m % 1440) / 60)
}

/// "1.4 GB", "730 MB"
pub fn size(b: u64) -> String {
    let g = b as f64 / 1e9;
    if g >= 1.0 { format!("{g:.1} GB") } else { format!("{:.0} MB", b as f64 / 1e6) }
}
