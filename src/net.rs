//! Background work: a small thread pool for HTTP + decoding. Finished work wakes the UI thread through
//! an SDL user event (SDL_PushEvent is thread-safe), so the UI can sleep while nothing happens.
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

type Job = Box<dyn FnOnce() + Send>;

pub struct Pool { tx: Sender<Job>, busy: Arc<std::sync::atomic::AtomicUsize> }

static WAKE_EVENT: OnceLock<u32> = OnceLock::new();

pub fn set_wake_event(t: u32) { let _ = WAKE_EVENT.set(t); }

/// Wake the UI loop (from any thread).
pub fn wake() {
    if let Some(&t) = WAKE_EVENT.get() {
        let mut e: crate::sys::sdl::Event = unsafe { std::mem::zeroed() };
        e.ty = t;
        unsafe { crate::sys::sdl::SDL_PushEvent(&mut e); }
    }
}

impl Pool {
    pub fn new(n: usize, name: &str) -> Pool {
        let busy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (tx, rx) = channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..n {
            let rx = rx.clone();
            std::thread::Builder::new().name(format!("{name}{i}")).spawn(move || loop {
                let job = { let g = rx.lock().unwrap(); g.recv() };
                match job { Ok(j) => j(), Err(_) => break }
            }).unwrap();
        }
        Pool { tx, busy }
    }
    pub fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        let b = self.busy.clone();
        b.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let _ = self.tx.send(Box::new(move || { f(); b.fetch_sub(1, std::sync::atomic::Ordering::AcqRel); }));
    }
    /// Wait (bounded) until queued and running jobs are done - e.g. the final playback report before the app exits.
    pub fn wait_idle(&self, max: Duration) {
        let until = std::time::Instant::now() + max;
        while self.busy.load(std::sync::atomic::Ordering::Acquire) > 0 && std::time::Instant::now() < until { std::thread::sleep(Duration::from_millis(20)); }
    }
}

/// DNS on the TV (inside the app jail) fails now and then when several lookups run at once, and a lookup can
/// also hang for ~5 s before failing (seen at app start). So: retry, keep the last good answer per host in memory,
/// and also on disk - at start the saved answer is used right away and refreshed in the background.
struct Resolver { cache: Mutex<std::collections::HashMap<String, (std::time::Instant, Vec<std::net::SocketAddr>)>> }

fn disk_path() -> String { format!("{}/dns-cache.json", crate::jf::DATA_DIR) }

fn disk_get(netloc: &str) -> Option<Vec<std::net::SocketAddr>> {
    let m: std::collections::HashMap<String, Vec<String>> = serde_json::from_str(&std::fs::read_to_string(disk_path()).ok()?).ok()?;
    let v: Vec<std::net::SocketAddr> = m.get(netloc)?.iter().filter_map(|a| a.parse().ok()).collect();
    (!v.is_empty()).then_some(v)
}

fn disk_put(netloc: &str, v: &[std::net::SocketAddr]) {
    let mut m: std::collections::HashMap<String, Vec<String>> =
        std::fs::read_to_string(disk_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let new: Vec<String> = v.iter().map(|a| a.to_string()).collect();
    if m.get(netloc) == Some(&new) { return; }
    m.insert(netloc.to_string(), new);
    if let Ok(s) = serde_json::to_string(&m) { let _ = std::fs::write(disk_path(), s); }
}

impl Resolver {
    fn lookup(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        let mut last = None;
        for attempt in 0..5u64 {
            match std::net::ToSocketAddrs::to_socket_addrs(netloc) {
                Ok(it) => {
                    let v: Vec<_> = it.collect();
                    if !v.is_empty() {
                        self.cache.lock().unwrap().insert(netloc.to_string(), (std::time::Instant::now(), v.clone()));
                        disk_put(netloc, &v);
                        return Ok(v);
                    }
                }
                Err(e) => { crate::log!("dns {netloc}: {e} (attempt {attempt})"); last = Some(e); }
            }
            std::thread::sleep(Duration::from_millis(if attempt == 0 { 20 } else { 150 * attempt }));
        }
        Err(last.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "dns")))
    }
}

impl ureq::Resolver for Resolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<std::net::SocketAddr>> {
        if let Some((at, v)) = self.cache.lock().unwrap().get(netloc) {
            if at.elapsed() < Duration::from_secs(600) { return Ok(v.clone()); }
        }
        // first use since start: trust the saved answer now, check it in the background
        if !self.cache.lock().unwrap().contains_key(netloc) {
            if let Some(v) = disk_get(netloc) {
                self.cache.lock().unwrap().insert(netloc.to_string(), (std::time::Instant::now(), v.clone()));
                let (me, host) = (RESOLVER.get().cloned(), netloc.to_string());
                std::thread::spawn(move || { if let Some(me) = me { let _ = me.lookup(&host); } });
                return Ok(v);
            }
        }
        match self.lookup(netloc) {
            Ok(v) => Ok(v),
            // stale is better than nothing
            Err(e) => self.cache.lock().unwrap().get(netloc).map(|(_, v)| v.clone()).ok_or(e),
        }
    }
}

static RESOLVER: OnceLock<Arc<Resolver>> = OnceLock::new();

struct Shared(Arc<Resolver>);
impl ureq::Resolver for Shared { fn resolve(&self, n: &str) -> std::io::Result<Vec<std::net::SocketAddr>> { self.0.resolve(n) } }

pub fn agent() -> ureq::Agent {
    let r = RESOLVER.get_or_init(|| Arc::new(Resolver { cache: Mutex::new(Default::default()) })).clone();
    ureq::AgentBuilder::new()
        .resolver(Shared(r))
        .timeout_connect(Duration::from_secs(8))
        .timeout_read(Duration::from_secs(20))
        .max_idle_connections_per_host(6)
        .user_agent("JellyfinNative/0.1 (webOS)")
        .build()
}

/// A typed mailbox for results coming back to the UI thread.
pub struct Mailbox<T> { pub tx: Sender<T>, rx: Receiver<T> }
impl<T> Mailbox<T> {
    pub fn new() -> Self { let (tx, rx) = channel(); Mailbox { tx, rx } }
    pub fn drain(&self) -> Vec<T> { self.rx.try_iter().collect() }
}
