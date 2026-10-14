//! Player facade for the UI (main thread). Sessions run on their own engine thread; a seek, a track
//! change or a quality change retires the running session and starts a new one once the old one has
//! released the decoder (LG's pipeline allows one main-video session at a time).
pub mod audio;
pub mod engine;
pub mod source;

use crate::log;
use crate::sys::smp;
use engine::{Obj, Phase, Request, Shared};
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Arc;

struct Session { shared: Arc<Shared>, obj: Arc<Obj>, window: CString, req: Request }

pub struct Player {
    agent: ureq::Agent,
    cur: Option<Session>,
    retiring: Vec<Session>,
    pending: Option<Request>,
    placed: Option<(u32, u32)>,
    paused: bool,
    /// where the last session stood when it was retired (for the position read-out during a reload)
    last_pos: i64,
}

impl Player {
    pub fn new(agent: ureq::Agent) -> Self {
        Player { agent, cur: None, retiring: vec![], pending: None, placed: None, paused: false, last_pos: 0 }
    }

    pub fn active(&self) -> bool { self.cur.is_some() || self.pending.is_some() }

    /// Start (or restart at a new position) playback.
    pub fn play(&mut self, mut req: Request) {
        // same file and tracks, only another position: the running engine seeks in place (keeps its connection and
        // demuxer, overlaps the pipeline reload with reading up to the new keyframe)
        if let Some(s) = &self.cur {
            let same = s.req.seekable && req.seekable && s.req.url == req.url && s.req.video_stream == req.video_stream
                && s.req.audio_stream == req.audio_stream && s.req.sub_stream == req.sub_stream;
            let alive = !s.shared.done.load(Ordering::Acquire) && !matches!(s.shared.phase(), Phase::Failed | Phase::Stopped)
                && s.shared.loaded.load(Ordering::Acquire);
            if same && alive && self.pending.is_none() {
                log!("player: in-place seek to {} ms", req.start_ns / 1_000_000);
                s.shared.seeking.store(true, Ordering::Release);
                s.shared.frames.store(0, Ordering::Relaxed);
                s.shared.position_ns.store(req.start_ns, Ordering::Relaxed);
                self.last_pos = req.start_ns;
                self.paused = false;
                s.shared.paused_req.store(false, Ordering::Relaxed);
                s.shared.seek_to.store(req.start_ns, Ordering::Release);
                return;
            }
        }
        self.retire();
        req.window_id.clear();
        self.last_pos = req.start_ns;
        self.paused = false;
        self.pending = Some(req);
        self.tick();
    }

    pub fn stop(&mut self) { self.pending = None; self.retire(); }

    fn retire(&mut self) {
        if let Some(s) = self.cur.take() {
            self.last_pos = s.shared.position_ns.load(Ordering::Relaxed);
            s.shared.stop.store(true, Ordering::Release);
            self.retiring.push(s);
        }
        self.placed = None;
    }

    /// Call every frame: reaps finished sessions, starts a pending one when the decoder is free, and
    /// places the video plane once the stream's raster is known.
    pub fn tick(&mut self) {
        self.retiring.retain(|s| {
            if s.shared.done.load(Ordering::Acquire) {
                unsafe { smp::vp_destroy(s.window.as_ptr()); }
                false
            } else { true }
        });
        if self.retiring.is_empty() {
            if let Some(mut req) = self.pending.take() {
                let mut id = [0 as std::ffi::c_char; 64];
                let ok = unsafe { smp::vp_create(id.as_mut_ptr(), 64) };
                let window = unsafe { std::ffi::CStr::from_ptr(id.as_ptr()) }.to_owned();
                log!("player: exported window {:?} ok={ok}", window);
                req.window_id = window.to_string_lossy().into_owned();
                let shared = Arc::new(Shared { stop: Arc::new(AtomicBool::new(false)), seek_to: std::sync::atomic::AtomicI64::new(i64::MIN), ..Default::default() });
                shared.paused_req.store(self.paused, Ordering::Relaxed);
                // events can outlive the session: the callback context is leaked on purpose
                std::mem::forget(shared.clone());
                let obj = Arc::new(Obj(AtomicPtr::new(std::ptr::null_mut())));
                engine::spawn(req.clone(), self.agent.clone(), shared.clone(), obj.clone());
                self.cur = Some(Session { shared, obj, window, req });
            }
        }
        if let Some(s) = &self.cur {
            let (w, h) = (s.shared.video_w.load(Ordering::Relaxed), s.shared.video_h.load(Ordering::Relaxed));
            if w > 0 && h > 0 && self.placed != Some((w, h)) {
                // letterbox/pillarbox inside the 1920x1080 UI space
                let (sw, sh) = (1920.0f32, 1080.0f32);
                let scale = (sw / w as f32).min(sh / h as f32);
                let (dw, dh) = ((w as f32 * scale).round() as i32, (h as f32 * scale).round() as i32);
                let rv = unsafe { smp::vp_place(s.window.as_ptr(), w as i32, h as i32, (1920 - dw) / 2, (1080 - dh) / 2, dw, dh) };
                log!("player: placed {w}x{h} -> {dw}x{dh} rv={rv}");
                self.placed = Some((w, h));
            }
        }
    }

    pub fn phase(&self) -> Phase {
        if self.pending.is_some() { return Phase::Opening; }
        self.cur.as_ref().map(|s| s.shared.phase()).unwrap_or(Phase::Stopped)
    }

    pub fn error(&self) -> Option<String> { self.cur.as_ref().and_then(|s| s.shared.error.lock().unwrap().clone()) }

    pub fn position_ns(&self) -> i64 {
        match &self.cur {
            Some(s) if s.shared.frames.load(Ordering::Relaxed) > 0 => s.shared.position_ns.load(Ordering::Relaxed),
            _ => self.last_pos,
        }
    }

    pub fn duration_ns(&self) -> i64 { self.cur.as_ref().map(|s| s.shared.duration_ns.load(Ordering::Relaxed)).unwrap_or(0) }
    pub fn has_picture(&self) -> bool { self.cur.as_ref().map_or(false, |s| s.shared.frames.load(Ordering::Relaxed) > 0) }
    pub fn info(&self) -> String { self.cur.as_ref().map(|s| s.shared.info.lock().unwrap().clone()).unwrap_or_default() }
    pub fn paused(&self) -> bool { self.paused }
    pub fn take_subs(&self) -> Vec<engine::SubEvent> {
        self.cur.as_ref().map(|s| s.shared.subs.lock().unwrap().drain(..).collect()).unwrap_or_default()
    }

    pub fn take_texts(&self) -> Vec<engine::TextEvent> {
        self.cur.as_ref().map(|s| std::mem::take(&mut *s.shared.texts.lock().unwrap())).unwrap_or_default()
    }

    pub fn set_paused(&mut self, p: bool) {
        self.paused = p;
        if let Some(s) = &self.cur {
            s.shared.paused_req.store(p, Ordering::Relaxed);
            let o = s.obj.0.load(Ordering::Acquire);
            if !o.is_null() && s.shared.loaded.load(Ordering::Acquire) {
                unsafe { if p { smp::smp_pause(o); } else { smp::smp_play(o); } }
            }
        }
    }

    /// Blocks (bounded) until every session has released the decoder; used on app exit/background.
    pub fn shutdown(&mut self) {
        self.stop();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !self.retiring.is_empty() && std::time::Instant::now() < until {
            self.tick();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
