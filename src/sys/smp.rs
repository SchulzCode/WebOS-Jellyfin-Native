//! FFI to src/native/smp.c (LG StarfishMediaAPIs + exported video window).
use std::ffi::{c_char, c_int, c_void};

pub type EventCb = extern "C" fn(ty: c_int, num: i64, s: *const c_char, ctx: *mut c_void);

extern "C" {
    pub fn smp_create() -> *mut c_void;
    pub fn smp_load(obj: *mut c_void, payload: *const c_char, cb: EventCb, ctx: *mut c_void) -> c_int;
    pub fn smp_feed(obj: *mut c_void, data: *const u8, size: u32, pts_ns: i64, es: c_int) -> c_char;
    pub fn smp_play(obj: *mut c_void) -> c_int;
    pub fn smp_pause(obj: *mut c_void) -> c_int;
    pub fn smp_unload(obj: *mut c_void) -> c_int;
    pub fn smp_push_eos(obj: *mut c_void) -> c_int;
    pub fn smp_destroy(obj: *mut c_void);
    pub fn vp_create(out: *mut c_char, cap: c_int) -> c_int;
    pub fn vp_place(id: *const c_char, sw: c_int, sh: c_int, x: c_int, y: c_int, w: c_int, h: c_int) -> c_int;
    pub fn vp_destroy(id: *const c_char);
}

// PF_EVENT_T (webOS 5+ numbering; webos-userland StarfishMediaAPIs.h)
pub const EV_FRAMEREADY: c_int = 0x00;
pub const EV_INT_ERROR: c_int = 0x12;
pub const EV_STR_ERROR: c_int = 0x13;
pub const EV_LOADCOMPLETED: c_int = 0x16;
pub const EV_UNLOADCOMPLETED: c_int = 0x17;
pub const EV_PLAYING: c_int = 0x1a;
pub const EV_PAUSED: c_int = 0x1b;
pub const EV_EOS: c_int = 0x1c;
pub const EV_BUFFERLOW: c_int = 0x2c;
pub const EV_BUFFERFULL: c_int = 0x2d;
