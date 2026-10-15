//! File logger: /tmp/jfnative.log, the previous run in /tmp/jfnative.log.1 (wiped at TV boot). Every line carries ms since app start.
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

static LOG: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();
static T0: OnceLock<Instant> = OnceLock::new();

pub fn init() {
    T0.get_or_init(Instant::now);
    // readable by this app only (it names the server and the user); /tmp is shared with every other app
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // keep the previous run's log (the run that just ended is usually the one to look at)
    let _ = std::fs::rename("/tmp/jfnative.log", "/tmp/jfnative.log.1");
    let f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open("/tmp/jfnative.log").ok();
    if let Some(f) = &f { let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600)); }
    let _ = LOG.set(Mutex::new(f));
}

pub fn ms() -> u64 { T0.get_or_init(Instant::now).elapsed().as_millis() as u64 }

pub fn write(msg: &str) {
    if let Some(m) = LOG.get() {
        if let Ok(mut g) = m.lock() {
            if let Some(f) = g.as_mut() { let _ = writeln!(f, "[{:>8}] {}", ms(), msg); }
        }
    }
}

#[macro_export]
macro_rules! log { ($($t:tt)*) => { $crate::log::write(&format!($($t)*)) } }
