//! The handful of SDL2 calls this app makes (LG's SDL 2.0.14 fork on the TV).
use std::ffi::{c_char, c_int, c_void};

pub const INIT_VIDEO: u32 = 0x20;
pub const INIT_EVENTS: u32 = 0x4000;
pub const WINDOW_FULLSCREEN: u32 = 0x1;
pub const WINDOW_OPENGL: u32 = 0x2;

pub const QUIT: u32 = 0x100;
pub const APP_WILLENTERBACKGROUND: u32 = 0x103;
pub const APP_DIDENTERBACKGROUND: u32 = 0x104;
pub const APP_WILLENTERFOREGROUND: u32 = 0x105;
pub const APP_DIDENTERFOREGROUND: u32 = 0x106;
pub const WINDOWEVENT: u32 = 0x200;
pub const KEYDOWN: u32 = 0x300;
pub const MOUSEMOTION: u32 = 0x400;
pub const MOUSEBUTTONDOWN: u32 = 0x401;
pub const MOUSEWHEEL: u32 = 0x403;

// SDL_WindowEventID
pub const WE_SHOWN: u8 = 1;
pub const WE_HIDDEN: u8 = 2;
pub const WE_FOCUS_GAINED: u8 = 12;
pub const WE_FOCUS_LOST: u8 = 13;

#[repr(C)] #[derive(Clone, Copy)] pub struct Keysym { pub scancode: i32, pub sym: i32, pub kmod: u16, pub unused: u32 }
#[repr(C)] #[derive(Clone, Copy)] pub struct KeyEv { pub ty: u32, pub ts: u32, pub win: u32, pub state: u8, pub repeat: u8, pub p2: u8, pub p3: u8, pub keysym: Keysym }
#[repr(C)] #[derive(Clone, Copy)] pub struct MotionEv { pub ty: u32, pub ts: u32, pub win: u32, pub which: u32, pub state: u32, pub x: i32, pub y: i32, pub xrel: i32, pub yrel: i32 }
#[repr(C)] #[derive(Clone, Copy)] pub struct ButtonEv { pub ty: u32, pub ts: u32, pub win: u32, pub which: u32, pub button: u8, pub state: u8, pub clicks: u8, pub p1: u8, pub x: i32, pub y: i32 }
#[repr(C)] #[derive(Clone, Copy)] pub struct WheelEv { pub ty: u32, pub ts: u32, pub win: u32, pub which: u32, pub x: i32, pub y: i32, pub direction: u32 }
#[repr(C)] #[derive(Clone, Copy)] pub struct WindowEv { pub ty: u32, pub ts: u32, pub win: u32, pub event: u8, pub p1: u8, pub p2: u8, pub p3: u8, pub data1: i32, pub data2: i32 }
#[repr(C)] #[derive(Clone, Copy)] pub struct UserEv { pub ty: u32, pub ts: u32, pub win: u32, pub code: i32, pub d1: *mut c_void, pub d2: *mut c_void }

#[repr(C)]
pub union Event { pub ty: u32, pub key: KeyEv, pub motion: MotionEv, pub button: ButtonEv, pub wheel: WheelEv, pub window: WindowEv, pub user: UserEv, pad: [u8; 56] }

extern "C" {
    pub fn SDL_Init(flags: u32) -> c_int;
    pub fn SDL_GL_SetAttribute(attr: c_int, value: c_int) -> c_int;
    pub fn SDL_CreateWindow(t: *const c_char, x: c_int, y: c_int, w: c_int, h: c_int, flags: u32) -> *mut c_void;
    pub fn SDL_GL_CreateContext(w: *mut c_void) -> *mut c_void;
    pub fn SDL_GL_SwapWindow(w: *mut c_void);
    pub fn SDL_GL_SetSwapInterval(i: c_int) -> c_int;
    pub fn SDL_GL_GetProcAddress(name: *const c_char) -> *mut c_void;
    pub fn SDL_PollEvent(e: *mut Event) -> c_int;
    pub fn SDL_WaitEventTimeout(e: *mut Event, ms: c_int) -> c_int;
    pub fn SDL_PushEvent(e: *mut Event) -> c_int;
    pub fn SDL_RegisterEvents(n: c_int) -> u32;
    pub fn SDL_GetError() -> *const c_char;
}

pub fn error() -> String { unsafe { std::ffi::CStr::from_ptr(SDL_GetError()).to_string_lossy().into_owned() } }
