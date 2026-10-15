//! Jellyfin Native: a native (Rust, GLES2, no browser) Jellyfin client for LG webOS TVs.
//! Video is decoded by the TV's own media pipeline; see player/engine.rs.
mod app;
mod feed;
mod gfx;
mod jf;
mod log;
mod net;
mod player;
mod sys;
mod ui;

pub const APP_ID: &str = "tld.my.jfnative";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the packaged assets (fonts) live: next to the binary.
pub fn assets_dir() -> String {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("assets").to_string_lossy().into_owned()))
        .unwrap_or_else(|| format!("/media/developer/apps/usr/palm/applications/{APP_ID}/assets"))
}

#[repr(C)]
struct RLimit { cur: std::ffi::c_ulong, max: std::ffi::c_ulong }
extern "C" {
    fn setrlimit(resource: std::ffi::c_int, rlim: *const RLimit) -> std::ffi::c_int;
    fn _exit(code: std::ffi::c_int) -> !;
}
const RLIMIT_CORE: std::ffi::c_int = 4;

fn main() {
    // A crash must never write a core file: on webOS it lands in the app's folder (~400 MB each) and fills the TV's
    // app storage.
    unsafe { setrlimit(RLIMIT_CORE, &RLimit { cur: 0, max: 0 }); }
    log::init();
    jf::secure_data_dir();
    let launch: serde_json::Value = std::env::args().nth(1).and_then(|a| serde_json::from_str(&a).ok()).unwrap_or_default();
    log!("start v{VERSION} launch={launch}");
    app::App::run(launch);
    // never linger: SAM considers the app closed only when the process is gone. _exit, not exit: our cleanup is done
    // (playback stopped and reported, SDL shut down), and LG's libraries crashed in their exit-time destructors.
    log!("bye");
    unsafe { _exit(0) }
}
