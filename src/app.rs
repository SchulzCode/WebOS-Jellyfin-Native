//! The application: SDL/GL setup, the screen stack, input, the player overlay, lifecycle.
//! Rendering is event-driven: a frame is drawn only when something moved, changed or arrived, so an idle
//! app (or a film playing without the OSD) costs nothing.
use crate::gfx::images::Images;
use crate::gfx::text::W;
use crate::gfx::{Color, Gfx, Rect};
use crate::jf::{Client, Item, Session};
use crate::net::{self, Pool};
use crate::player::Player;
use crate::sys::sdl;
use crate::ui::player_ui::PlayerUi;
use crate::ui::{self, Action, Cx, Hot, Key, Screen};
use serde::{Deserialize, Serialize};
use std::ffi::CString;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// OLED care: idle screen after this long without input in the menus (LG's screensaver never starts over this app's
/// menus - tested) / with a paused video (LG's own screensaver would start there after ~3 min; Minimal Home's helper
/// switches it off while this app is in front, so this is the only one); the app closes after IDLE_QUIT more and the
/// launcher (idle screen, then panel off) takes over. A playing video counts as activity.
const IDLE_MENU: Duration = Duration::from_secs(120);
const IDLE_PAUSED: Duration = Duration::from_secs(180);
const IDLE_QUIT: Duration = Duration::from_secs(20 * 60);
/// Held arrows / FF / RW: the remote repeats every ~50 ms, far too fast to follow - act on one repeat per this long.
const REPEAT_EVERY: Duration = Duration::from_millis(80);

// ---- preferences ----
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Prefs { pub sub_size: usize, pub subs_auto: bool, pub audio_lang: String, pub autoplay: bool, pub stats: bool, pub remember_tracks: bool }
impl Default for Prefs { fn default() -> Self { Prefs { sub_size: 1, subs_auto: true, audio_lang: String::new(), autoplay: true, stats: false, remember_tracks: true } } }

static PREFS: OnceLock<Mutex<Prefs>> = OnceLock::new();
fn prefs_cell() -> &'static Mutex<Prefs> {
    PREFS.get_or_init(|| Mutex::new(std::fs::read_to_string(format!("{}/prefs.json", crate::jf::DATA_DIR)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()))
}
pub fn prefs() -> Prefs { prefs_cell().lock().unwrap().clone() }
pub fn set_prefs(p: Prefs) {
    let _ = std::fs::create_dir_all(crate::jf::DATA_DIR);
    let _ = std::fs::write(format!("{}/prefs.json", crate::jf::DATA_DIR), serde_json::to_string(&p).unwrap());
    *prefs_cell().lock().unwrap() = p;
}

// ---- what to play ----
#[derive(Clone, Debug)]
pub enum Start { Resume, Begin }

#[derive(Clone, Debug)]
pub struct PlaySpec { pub item: Item, pub start: Start, pub queue: Vec<Item>, /** chosen version (media source id) */ pub source: Option<String> }
impl PlaySpec {
    pub fn resume(item: Item) -> PlaySpec { PlaySpec { item, start: Start::Resume, queue: vec![], source: None } }
    pub fn from_start(item: Item) -> PlaySpec { PlaySpec { item, start: Start::Begin, queue: vec![], source: None } }
    pub fn with_queue(mut self, q: Vec<Item>) -> PlaySpec { self.queue = q; self }
    pub fn with_source(mut self, s: Option<String>) -> PlaySpec { self.source = s; self }
}

struct Toast { text: String, until: Instant }

/// Frame timing, logged every 2 s while frames are being drawn: [update, draw, submit, swap] in µs.
#[derive(Default)]
struct FrameStats { since: Option<Instant>, n: u32, sum: [u64; 4], max: [u64; 4], draws: u32, verts: u32, flush: u64, text: u64 }

impl FrameStats {
    fn add(&mut self, t: [u64; 4], draws: u32, verts: u32, flush: u64, text: u64) {
        self.flush += flush; self.text += text;
        let since = *self.since.get_or_insert_with(Instant::now);
        self.n += 1; self.draws += draws; self.verts += verts;
        for i in 0..4 { self.sum[i] += t[i]; self.max[i] = self.max[i].max(t[i]); }
        if since.elapsed() >= Duration::from_secs(2) {
            let n = self.n as u64;
            let f = |i: usize| format!("{:.1}/{:.1}", self.sum[i] as f64 / n as f64 / 1000.0, self.max[i] as f64 / 1000.0);
            crate::log!("fps {:.0}: update {} draw {} submit {} swap {} ms (avg/max), {} draws, {} verts, flush {:.1} text-fit {:.1} ms",
                n as f64 / since.elapsed().as_secs_f64(), f(0), f(1), f(2), f(3), self.draws / self.n, self.verts / self.n, self.flush as f64 / n as f64 / 1000.0, self.text as f64 / n as f64 / 1000.0);
            *self = FrameStats::default();
        }
    }
}

pub struct App {
    win: *mut std::ffi::c_void,
    g: Gfx,
    images: Images,
    pool: Arc<Pool>,
    agent: ureq::Agent,
    client: Option<Client>,
    stack: Vec<Box<dyn Screen>>,
    player: Player,
    pui: Option<PlayerUi>,
    hot: Vec<(Rect, Hot)>,
    t0: Instant,
    last: Instant,
    dirty: bool,
    toast: Option<Toast>,
    cursor: Option<(f32, f32, Instant)>,
    launch_params: serde_json::Value,
    /// Started from a launcher card to play one item: leaving the player closes the app (back to the launcher).
    exit_after_play: bool,
    /// OLED care (LG's own screensaver never starts over this native app - tested): idle screen after a while without input
    last_input: Instant,
    idle: ui::Anim,
    background: bool,
    quit: bool,
    last_clock: String,
    stats: FrameStats,
    animating: bool,
    pending_action: Option<Action>,
    /// when the last remote key repeat was acted on; and whether the held key's first press only woke the idle screen
    last_repeat: Instant,
    held_woke: bool,
    dev_keys: bool,
    gpu_timing: bool,
}

impl App {
    pub fn run(launch: serde_json::Value) {
        unsafe {
            std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_BACK", "true");
            std::env::set_var("SDL_WEBOS_ACCESS_POLICY_KEYS_EXIT", "true");
            if sdl::SDL_Init(sdl::INIT_VIDEO | sdl::INIT_EVENTS) != 0 { crate::log!("SDL_Init: {}", sdl::error()); return; }
            sdl::SDL_GL_SetAttribute(21, 4); // ES profile
            sdl::SDL_GL_SetAttribute(17, 2);
            sdl::SDL_GL_SetAttribute(18, 0);
            sdl::SDL_GL_SetAttribute(3, 8); // alpha: the video plane shows through
            let t = CString::new("Jellyfin").unwrap();
            let win = sdl::SDL_CreateWindow(t.as_ptr(), 0, 0, 1920, 1080, sdl::WINDOW_OPENGL | sdl::WINDOW_FULLSCREEN);
            if win.is_null() { crate::log!("window: {}", sdl::error()); return; }
            if sdl::SDL_GL_CreateContext(win).is_null() { crate::log!("gl: {}", sdl::error()); return; }
            sdl::SDL_GL_SetSwapInterval(1);
            let gl = glow::Context::from_loader_function(|n| { let c = CString::new(n).unwrap(); sdl::SDL_GL_GetProcAddress(c.as_ptr()) as *const _ });
            let wake = sdl::SDL_RegisterEvents(1);
            net::set_wake_event(wake);
            ui::init_tz();
            let agent = net::agent();
            let mut app = App {
                win, g: Gfx::new(gl, 1920.0, 1080.0), images: Images::new(agent.clone()), pool: Arc::new(Pool::new(4, "net")), agent: agent.clone(),
                client: None, stack: vec![], player: Player::new(agent), pui: None, hot: vec![], t0: Instant::now(), last: Instant::now(),
                dirty: true, toast: None, cursor: None, launch_params: launch, exit_after_play: false, last_input: Instant::now(), idle: ui::Anim::new(0.0, 0.7), background: false, quit: false, last_clock: String::new(), pending_action: None, stats: FrameStats::default(), animating: false, last_repeat: Instant::now(), held_woke: false,
                dev_keys: cfg!(feature = "dev") && std::path::Path::new(&format!("{}/dev-keys-enabled", crate::jf::DATA_DIR)).exists(),
                gpu_timing: cfg!(feature = "dev") && std::path::Path::new(&format!("{}/gpu-timing", crate::jf::DATA_DIR)).exists(),
            };
            crate::log!("ui ready after {} ms", crate::log::ms());
            app.start();
            app.main_loop();
            app.player.shutdown();
            if let Some(p) = app.pui.as_mut() { p.on_exit(&app.client); }
            // the final playback report (resume position) is sent from the pool: let it finish
            app.pool.wait_idle(Duration::from_secs(2));
            crate::log!("exit");
            // No SDL_Quit and no teardown of the GL objects: the process ends right after this (main's _exit), and
            // tearing down LG's SDL/EGL while our textures still exist crashed (a ~400 MB core file per exit).
            std::mem::forget(app);
        }
    }

    fn cx<'a>(images: &'a mut Images, client: &'a Option<Client>, pool: &'a Arc<Pool>, hot: &'a mut Vec<(Rect, Hot)>, dt: f32, t: f32) -> Cx<'a> {
        Cx { images, client, pool, dt, t, hot }
    }

    fn start(&mut self) {
        let agent = self.agent.clone();
        match Session::load() {
            Some(s) => {
                crate::log!("signed in as {} ({})", s.user_name, s.server);
                self.client = Some(Client { s, agent });
                let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, 0.0);
                self.stack = vec![Box::new(ui::home::Home::new(&mut cx))];
                self.handle_launch();
            }
            None => {
                let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, 0.0);
                self.stack = vec![Box::new(ui::connect::Connect::new(agent, &mut cx))];
            }
        }
    }

    /// Launch parameters: {"itemId": "...", "play": true} opens (and plays) an item - e.g. from Minimal Home.
    fn handle_launch(&mut self) {
        let p = self.launch_params.get("parameters").cloned().unwrap_or_default();
        let Some(id) = p.get("itemId").and_then(|v| v.as_str()).map(|s| s.to_string()) else { return };
        let play = p.get("play").and_then(|v| v.as_bool()).unwrap_or(false);
        crate::log!("launch: item {id} play={play}");
        let item = Item { id, ..Default::default() };
        if play { self.exit_after_play = true; self.act(Action::Play(PlaySpec::resume(item))); }
        else {
            let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, 0.0);
            let d = ui::detail::Detail::new(item, &mut cx);
            self.stack.push(Box::new(d));
        }
    }

    fn act(&mut self, a: Action) {
        let t = self.t0.elapsed().as_secs_f32();
        match a {
            Action::None => return,
            Action::Push(s) => self.stack.push(s),
            Action::Pop => {
                if self.stack.len() > 1 { self.stack.pop(); }
                let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, t);
                if let Some(s) = self.stack.last_mut() { s.resumed(&mut cx); }
            }
            Action::Play(spec) => {
                if let Some(c) = self.client.clone() {
                    if let Some(mut old) = self.pui.take() { old.on_exit(&self.client); }
                    self.pui = Some(PlayerUi::start(spec, c, &mut self.player, &self.pool));
                }
            }
            Action::SignedIn(s) => {
                s.save();
                self.client = Some(Client { s, agent: self.agent.clone() });
                let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, t);
                self.stack = vec![Box::new(ui::home::Home::new(&mut cx))];
                self.show_toast("Signed in — welcome");
            }
            Action::SignOut => {
                Session::clear();
                self.client = None;
                let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, t);
                self.stack = vec![Box::new(ui::connect::Connect::new(self.agent.clone(), &mut cx))];
            }
            Action::Exit => self.quit = true,
            Action::Toast(s) => self.show_toast(&s),
        }
        self.dirty = true;
    }

    fn close_player(&mut self, dt: f32, t: f32) {
        let Some(mut done) = self.pui.take() else { return };
        done.on_exit(&self.client);
        if self.exit_after_play { crate::log!("launch: player closed, exiting to the launcher"); self.quit = true; return; }
        let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, dt, t);
        if let Some(s) = self.stack.last_mut() { s.resumed(&mut cx); }
    }

    fn show_toast(&mut self, s: &str) { self.toast = Some(Toast { text: s.into(), until: Instant::now() + Duration::from_millis(2600) }); }

    /// Any input: restarts the idle timer. Returns true if it only woke the screen (the press is not acted on).
    fn wake(&mut self) -> bool {
        self.last_input = Instant::now();
        if self.idle.to > 0.5 || self.idle.v > 0.02 {
            self.idle.set(0.0);
            self.dirty = true;
            crate::log!("idle: woken");
            return true;
        }
        false
    }

    fn key(&mut self, k: Key) {
        let t = self.t0.elapsed().as_secs_f32();
        self.dirty = true;
        if self.wake() { return; }
        ui::set_pointer(None);
        if let Some(p) = self.pui.as_mut() {
            let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, t);
            let a = p.key(k, &mut self.player, &mut cx);
            if p.finished() { self.close_player(0.0, t); }
            self.act(a);
            return;
        }
        let a = {
            let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, t);
            match self.stack.last_mut() { Some(s) => s.key(k, &mut cx), None => Action::None }
        };
        self.act(a);
    }

    fn pointer(&mut self, x: f32, y: f32, click: bool) {
        // the Magic Remote reports small hand tremor all the time; only a real move counts
        let min = if self.pui.is_some() { 24.0 } else { 4.0 };
        let moved = self.cursor.map_or(true, |(px, py, _)| (px - x).abs() + (py - y).abs() > min);
        if !moved && !click { return; }
        self.cursor = Some((x, y, Instant::now()));
        if self.wake() { return; }
        let hit = self.hot.iter().rev().find(|(r, _)| r.contains(x, y)).map(|(_, h)| *h);
        if click { crate::log!("click {x:.0},{y:.0} hit={}", hit.is_some()); }
        ui::set_pointer(Some((x, y)));
        if let Some(p) = self.pui.as_mut() {
            let k = p.pointer(x, click, hit, &mut self.player);
            if let Some(k) = k { self.key(k); ui::set_pointer(Some((x, y))); }
            self.dirty = true;
            return;
        }
        if let Some(h) = hit {
            if let Some(s) = self.stack.last_mut() { if s.hover(h) { self.dirty = true; } }
            if click { self.key(Key::Ok); ui::set_pointer(Some((x, y))); }
        }
    }

    unsafe fn main_loop(&mut self) {
        let mut e: sdl::Event = std::mem::zeroed();
        while !self.quit {
            // Animating: sleep to the next 60 Hz slot, then take whatever input came in. The swap doesn't block
            // on this compositor and SDL_WaitEventTimeout oversleeps (~22 ms for 16), so pace by hand.
            // Idle: block until input or a wake-up event.
            let mut have = if self.dirty { sdl::SDL_PollEvent(&mut e) }
                else if self.animating {
                    let next = self.last + Duration::from_micros(16_667);
                    let now = Instant::now();
                    if next > now { std::thread::sleep(next - now); }
                    sdl::SDL_PollEvent(&mut e)
                }
                else { sdl::SDL_WaitEventTimeout(&mut e, if self.pui.as_ref().map_or(false, |p| p.wants_ticks()) { 33 } else { 250 }) };
            let t0 = Instant::now();
            let mut n = 0;
            while have != 0 {
                self.event(&e);
                n += 1;
                have = sdl::SDL_PollEvent(&mut e);
            }
            if self.quit { break; }
            if self.dev_keys { self.dev_input(); }
            let t1 = Instant::now();
            self.frame();
            let (ev, fr) = (t1 - t0, t1.elapsed());
            if ev + fr > Duration::from_millis(60) {
                crate::log!("slow: events {} ms ({n}), frame {} ms, screen {}", ev.as_millis(), fr.as_millis(),
                    if self.pui.is_some() { "player" } else { self.stack.last().map_or("-", |s| s.name()) });
            }
        }
    }

    unsafe fn event(&mut self, e: &sdl::Event) {
        match e.ty {
            sdl::QUIT => self.quit = true,
            sdl::KEYDOWN => {
                let k = e.key.keysym;
                let repeat = k.scancode == ui::REMOTE_REPEAT || e.key.repeat != 0;
                if !repeat { crate::log!("key sc={} sym={} rep={}", k.scancode, k.sym, e.key.repeat); }
                let Some(key) = ui::map_key(k.scancode, k.sym) else { return };
                if repeat {
                    // holding a button: only movement repeats (not OK, Back, play/pause), at a pace one can follow,
                    // and not when the first press only woke the idle screen
                    if self.held_woke || !matches!(key, Key::Up | Key::Down | Key::Left | Key::Right | Key::Ff | Key::Rw) { return; }
                    if self.last_repeat.elapsed() < REPEAT_EVERY { return; }
                    self.last_repeat = Instant::now();
                } else {
                    self.held_woke = self.idle.to > 0.5 || self.idle.v > 0.02;
                }
                self.key(key);
            }
            sdl::MOUSEMOTION => self.pointer(e.motion.x as f32, e.motion.y as f32, false),
            sdl::MOUSEBUTTONDOWN => if e.button.button == 1 { self.pointer(e.button.x as f32, e.button.y as f32, true) },
            sdl::MOUSEWHEEL => { let k = if e.wheel.y > 0 { Key::Up } else { Key::Down }; self.key(k); }
            sdl::APP_WILLENTERBACKGROUND | sdl::APP_DIDENTERBACKGROUND => self.go_background(),
            sdl::APP_WILLENTERFOREGROUND | sdl::APP_DIDENTERFOREGROUND => self.go_foreground(),
            sdl::WINDOWEVENT => {
                let we = e.window.event;
                crate::log!("window event {we}");
                if we == sdl::WE_HIDDEN || we == sdl::WE_FOCUS_LOST { self.go_background(); }
                if we == sdl::WE_SHOWN || we == sdl::WE_FOCUS_GAINED { self.go_foreground(); }
                self.dirty = true;
            }
            _ => self.dirty = true, // our wake-up event (network/image results) and anything else
        }
    }

    /// Development only (enabled by the file dev-keys-enabled): key names written to DATA_DIR/keys are
    /// played back as remote presses, so the UI can be driven over SSH.
    fn dev_input(&mut self) {
        let p = format!("{}/keys", crate::jf::DATA_DIR);
        let Ok(s) = std::fs::read_to_string(&p) else { return };
        let _ = std::fs::remove_file(&p);
        for w in s.split_whitespace() {
            // "click:x:y" / "move:x:y" = Magic Remote pointer
            if let Some(rest) = w.strip_prefix("click:").map(|r| (r, true)).or_else(|| w.strip_prefix("move:").map(|r| (r, false))) {
                let v: Vec<f32> = rest.0.split(':').filter_map(|n| n.parse().ok()).collect();
                if v.len() == 2 { crate::log!("dev pointer {w}"); self.cursor = None; self.pointer(v[0], v[1], rest.1); }
                continue;
            }
            let k = match w { "U" => Key::Up, "D" => Key::Down, "L" => Key::Left, "R" => Key::Right, "OK" => Key::Ok, "B" => Key::Back,
                "PLAY" => Key::Play, "PAUSE" => Key::Pause, "FF" => Key::Ff, "RW" => Key::Rw, "STOP" => Key::Stop, "INFO" => Key::Info, "CHU" => Key::ChUp, "CHD" => Key::ChDown,
                w if w.len() == 1 => Key::Char(w.chars().next().unwrap()), _ => continue };
            crate::log!("dev key {w}");
            self.key(k);
        }
    }

    fn go_background(&mut self) {
        if self.background { return; }
        self.background = true;
        crate::log!("lifecycle: background");
        // give the decoder back right away (other apps need it); remember where we were
        if let Some(p) = self.pui.as_mut() { p.suspend(&mut self.player, &self.client); }
    }

    fn go_foreground(&mut self) {
        if !self.background { return; }
        self.background = false;
        crate::log!("lifecycle: foreground");
        // whatever brought us back (the key that ended LG's screensaver, a launch) counts as input: no idle screen right away
        self.last_input = Instant::now();
        if let Some(p) = self.pui.as_mut() { p.resume_after_background(&mut self.player); }
        let t = self.t0.elapsed().as_secs_f32();
        let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, 0.0, t);
        if self.pui.is_none() { if let Some(s) = self.stack.last_mut() { s.resumed(&mut cx); } }
        self.dirty = true;
    }

    unsafe fn frame(&mut self) {
        // the server rejected our token (signed out from the dashboard, password changed): back to sign-in
        if crate::jf::AUTH_LOST.swap(false, std::sync::atomic::Ordering::AcqRel) & self.client.is_some() {
            crate::log!("auth: token rejected, back to sign-in");
            if let Some(mut p) = self.pui.take() { p.on_exit(&None); }
            self.player.stop();
            self.act(Action::SignOut);
            self.show_toast("Your sign-in is no longer valid - please sign in again");
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32().min(0.05);
        self.last = now;
        let t = self.t0.elapsed().as_secs_f32();
        self.player.tick();
        let mut busy = self.images.pump(&self.g) | self.images.is_fading();
        // screen-level async actions (sign-in finished)
        if let Some(s) = self.stack.last_mut() {
            let a = s.poll_action();
            if !matches!(a, Action::None) { self.act(a); busy = true; }
        }
        let playing = self.pui.is_some();
        {
            let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, dt, t);
            // never in the background: a pending seek, up-next autoplay or a late item would start a pipeline
            // (and play) while another app or LG's screensaver is in front
            if !self.background {
                if let Some(p) = self.pui.as_mut() {
                    let (b, a) = p.update(&mut self.player, &mut cx);
                    busy |= b;
                    if !matches!(a, Action::None) { self.pending_action = Some(a); }
                } else if let Some(s) = self.stack.last_mut() {
                    busy |= s.update(&mut cx);
                }
            }
        }
        if let Some(a) = self.pending_action.take() { self.act(a); busy = true; }
        if let Some(p) = &self.pui {
            if p.finished() { self.close_player(dt, t); busy = true; }
        }
        let clock = ui::hhmm(0);
        if clock != self.last_clock { self.last_clock = clock; busy = true; }
        // OLED idle screen: menus after IDLE_MENU, a paused/failed player after IDLE_PAUSED; after IDLE_QUIT more the app
        // closes, so the launcher (its own idle screen, then panel off) takes over
        // a video playing (or loading) is activity: without this, a binge without key presses was closed between episodes
        if self.pui.as_ref().map_or(false, |p| !p.idle_ok(&self.player)) { self.last_input = now; }
        let allowed = self.pui.as_ref().map_or(true, |p| p.idle_ok(&self.player)) && !self.background;
        let limit = if self.pui.is_some() && self.player.paused() { IDLE_PAUSED } else { IDLE_MENU };
        let idle_for = self.last_input.elapsed();
        let want = allowed && idle_for > limit;
        if want && self.idle.to < 0.5 { crate::log!("idle: screen to black after {} s", limit.as_secs()); }
        self.idle.set(if want { 1.0 } else { 0.0 });
        busy |= self.idle.step(dt);
        if want && idle_for > limit + IDLE_QUIT { crate::log!("idle: {} min without input, closing", (limit + IDLE_QUIT).as_secs() / 60); self.quit = true; }
        if let Some(to) = &self.toast { if now > to.until { self.toast = None; } busy = true; }
        self.animating = busy;
        // nothing to show while another app / LG's screensaver is in front
        if self.background { self.animating = false; self.stats.since = None; return; }
        if !(busy || self.dirty) { self.stats.since = None; return; }
        self.dirty = false;
        let t_upd = now.elapsed();

        // draw
        self.hot.clear();
        let video_mode = playing && self.pui.as_ref().map_or(false, |p| p.video_visible(&self.player));
        if video_mode {
            self.g.begin(Color(0.0, 0.0, 0.0, 0.0));
        } else {
            self.g.begin(ui::BG);
        }
        {
            let mut cx = Self::cx(&mut self.images, &self.client, &self.pool, &mut self.hot, dt, t);
            if let Some(p) = self.pui.as_mut() { p.draw(&mut self.g, &self.player, &mut cx); }
            else if let Some(s) = self.stack.last_mut() { s.draw(&mut self.g, &mut cx); }
        }
        if let Some(to) = &self.toast {
            let a = ((to.until - now).as_secs_f32() / 0.3).min(1.0).min((2.6 - (to.until - now).as_secs_f32()) / 0.2).clamp(0.0, 1.0);
            let w = self.g.text.measure(&to.text, 22.0, W::Medium, 0.0) + 56.0;
            let r = Rect::new(960.0 - w / 2.0, 990.0 - 8.0 * a, w, 52.0);
            self.g.alpha = a;
            self.g.rect(r, Color(0.16, 0.16, 0.2, 0.92), 26.0);
            self.g.text_c(960.0, r.y + 13.0, 22.0, W::Medium, ui::TEXT, &to.text);
            self.g.alpha = 1.0;
        }
        // idle screen: true black (pixels off), a dim clock that moves every minute, dimmer still after 10 minutes
        let ia = ui::smooth(self.idle.v);
        if ia > 0.01 {
            self.g.rect(Rect::new(0.0, 0.0, 1920.0, 1080.0), Color(0.0, 0.0, 0.0, ia), 0.0);
            let m = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() / 60).unwrap_or(0);
            let (x, y) = (140.0 + ((m * 7919) % 1100) as f32, 120.0 + ((m * 104_729) % 640) as f32);
            let deep = self.last_input.elapsed() > Duration::from_secs(600) + IDLE_MENU;
            let k = if deep { 0.5 } else { 1.0 } * ia;
            self.g.text(x, y, 150.0, W::Regular, ui::TEXT.a(0.30 * k), &ui::hhmm(0));
            let line = self.pui.as_ref().map(|p| p.idle_line()).unwrap_or_else(|| "Jellyfin".into());
            self.g.text(x + 8.0, y + 180.0, 26.0, W::Regular, ui::TEXT.a(0.18 * k), &line);
        }
        let t_draw = now.elapsed();
        self.g.end();
        if self.gpu_timing { use glow::HasContext; self.g.gl.finish(); } // diagnostics: makes "submit" the GPU time
        let t_sub = now.elapsed();
        sdl::SDL_GL_SwapWindow(self.win);
        let t_swap = now.elapsed();
        let us = |d: Duration| d.as_micros() as u64;
        self.stats.add([us(t_upd), us(t_draw - t_upd), us(t_sub - t_draw), us(t_swap - t_sub)], self.g.draws, self.g.verts_drawn, self.g.flush_us, self.g.text.prof_us);
    }
}
