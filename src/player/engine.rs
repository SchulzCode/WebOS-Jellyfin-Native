#![allow(non_upper_case_globals)]
//! One playback session = one engine thread: HTTP -> libavformat (the TV's own FFmpeg 5.0) -> access units
//! -> LG's media pipeline (StarfishMediaAPIs "BUFFERSTREAM"), whose decoded picture lands on the exported
//! video window under our GL surface. Seeking is a fresh session at the new position (the pipeline's
//! in-place seek needs private struct offsets on this firmware; a reload is ~1 s and always works).
//!
//! The engine thread owns the pipeline object for its whole life: create, Load, Play, feed, and on stop
//! Unload -> wait UNLOADCOMPLETED -> destroy. The main thread only flips flags and reads `Shared`.
use super::source::HttpSource;
use crate::log;
use crate::sys::ff::*;
use crate::sys::smp;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const AVERROR_EOF: c_int = -541478725;
const AVERROR_EXIT: c_int = -1414092869;
const AVERROR_EAGAIN: c_int = -11;
const AVSEEK_SIZE: c_int = 0x10000;
const AV_PKT_FLAG_KEY: c_int = 1;
const NOPTS: i64 = i64::MIN;
const ES_VIDEO: c_int = 1;
/// a keyframe further than this before the wanted position is decoded through instead of shown
const PRECISE_SEEK_NS: i64 = 2_500_000_000;
const ES_AUDIO: c_int = 2;

#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub auth: String,
    /// direct play of the original file (range requests); false = a server transcode stream
    pub seekable: bool,
    /// where to start, in the item's own timeline
    pub start_ns: i64,
    /// for transcodes started at StartTimeTicks: stream time 0 == this position
    pub offset_ns: i64,
    pub video_stream: Option<i32>,
    pub audio_stream: Option<i32>,
    /// an embedded image subtitle track (PGS/DVD) to decode here and hand to the UI as bitmaps
    pub sub_stream: Option<i32>,
    pub window_id: String,
    /// lip sync for Bluetooth headphones: audio is handed to the pipeline this much earlier than its picture
    pub audio_lead_ns: i64,
}

/// One text subtitle event from an embedded track (SRT/ASS/...), demuxed here instead of extracted by the server.
/// `text` is the raw event text (ASS override tags still in); `stream` is the file's stream index.
pub struct TextEvent { pub stream: i32, pub start_ns: i64, pub end_ns: i64, pub text: String, pub ass: bool }

/// One decoded image subtitle: show from `start_ns` (item timeline) until the next event or `end_ns`.
pub struct SubEvent { pub start_ns: i64, pub end_ns: Option<i64>, pub canvas: (i32, i32), pub rects: Vec<SubRect> }
pub struct SubRect { pub x: i32, pub y: i32, pub w: i32, pub h: i32, pub rgba: Vec<u8> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Opening, Loading, Playing, Paused, Ended, Failed, Stopped }

#[derive(Default)]
pub struct Shared {
    pub phase: Mutex<Option<Phase>>,
    pub error: Mutex<Option<String>>,
    pub position_ns: AtomicI64,
    pub duration_ns: AtomicI64,
    pub frames: AtomicU64,
    pub video_w: AtomicU32,
    pub video_h: AtomicU32,
    pub loaded: AtomicBool,
    pub unloaded: AtomicBool,
    pub eos: AtomicBool,
    pub stop: Arc<AtomicBool>,
    pub done: AtomicBool,
    pub paused_req: AtomicBool,
    pub offset_ns: AtomicI64,
    pub last_frame_at: Mutex<Option<Instant>>,
    pub info: Mutex<String>,
    pub subs: Mutex<VecDeque<SubEvent>>,
    pub texts: Mutex<Vec<TextEvent>>,
    /// in-place seek request (item timeline, ns; NOPTS = none), set by the main thread
    pub seek_to: AtomicI64,
    /// between a seek request and the new pipeline's loadCompleted: the old pipeline's late events are ignored
    pub seeking: AtomicBool,
    /// audio lead (see Request::audio_lead_ns); an in-place seek may change it, it applies from the next pipeline on
    pub audio_lead_ns: AtomicI64,
}

impl Shared {
    pub fn phase(&self) -> Phase { self.phase.lock().unwrap().unwrap_or(Phase::Opening) }
    fn set_phase(&self, p: Phase) { *self.phase.lock().unwrap() = Some(p); }
    fn fail(&self, msg: String) {
        log!("player: FAILED {msg}");
        *self.error.lock().unwrap() = Some(msg);
        self.set_phase(Phase::Failed);
    }
}

/// The pipeline object pointer, shared so the main thread can pause/resume it.
pub struct Obj(pub AtomicPtr<c_void>);

extern "C" fn on_event(ty: c_int, num: i64, s: *const c_char, ctx: *mut c_void) {
    // ctx is an Arc<Shared> leaked for the life of the process (events can arrive after teardown)
    let sh = unsafe { &*(ctx as *const Shared) };
    let text = if s.is_null() { String::new() } else { unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned() };
    match ty {
        smp::EV_FRAMEREADY => {
            if sh.seeking.load(Ordering::Acquire) { return; }
            sh.position_ns.store(num + sh.offset_ns.load(Ordering::Relaxed), Ordering::Relaxed);
            if sh.frames.fetch_add(1, Ordering::Relaxed) == 0 { log!("smp: first frame pts={num}"); }
            *sh.last_frame_at.lock().unwrap() = Some(Instant::now());
            return;
        }
        smp::EV_LOADCOMPLETED => { sh.loaded.store(true, Ordering::Release); }
        smp::EV_UNLOADCOMPLETED => { sh.unloaded.store(true, Ordering::Release); }
        smp::EV_PLAYING => { if sh.phase() != Phase::Failed { sh.set_phase(Phase::Playing); } }
        smp::EV_PAUSED => { if sh.phase() == Phase::Playing { sh.set_phase(Phase::Paused); } }
        smp::EV_EOS => { if !sh.seeking.load(Ordering::Acquire) { sh.eos.store(true, Ordering::Release); } }
        smp::EV_INT_ERROR | smp::EV_STR_ERROR if sh.seeking.load(Ordering::Acquire) => {}
        smp::EV_INT_ERROR | smp::EV_STR_ERROR => {
            // a refusal before any picture is fatal (e.g. 601 Resource Allocation Error)
            if sh.frames.load(Ordering::Relaxed) == 0 { sh.fail(format!("TV pipeline error {num} {text}")); }
        }
        _ => {}
    }
    if ty != smp::EV_BUFFERFULL && ty != smp::EV_BUFFERLOW { log!("smp_cb type={ty} num={num} str={}", text.chars().take(240).collect::<String>()); }
}

struct Io { src: HttpSource }

extern "C" fn io_read(opaque: *mut c_void, buf: *mut u8, size: c_int) -> c_int {
    let io = unsafe { &mut *(opaque as *mut Io) };
    let b = unsafe { std::slice::from_raw_parts_mut(buf, size as usize) };
    match io.src.read(b) {
        Ok(0) => AVERROR_EOF,
        Ok(n) => n as c_int,
        Err(e) => { if !io.src.stop.load(Ordering::Relaxed) { log!("source: {e}"); } AVERROR_EXIT }
    }
}

extern "C" fn io_seek(opaque: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let io = unsafe { &mut *(opaque as *mut Io) };
    if whence & AVSEEK_SIZE != 0 { return io.src.len.map(|l| l as i64).unwrap_or(-1); }
    if !io.src.seekable { return -1; }
    let base = match whence & 3 { 0 => 0, 2 => match io.src.len { Some(l) => l as i64, None => return -1 }, _ => return -1 };
    match io.src.seek((base + offset).max(0) as u64) { Ok(p) => p as i64, Err(_) => -1 }
}

/// What the Load payload declares; see `payload()`.
struct Decl { video: &'static str, audio: Option<&'static str>, w: i32, h: i32, fps: (i32, i32) }

fn video_codec(id: AVCodecID) -> Option<&'static str> {
    match id {
        AVCodecID_AV_CODEC_ID_H264 => Some("H264"),
        AVCodecID_AV_CODEC_ID_HEVC => Some("H265"),
        AVCodecID_AV_CODEC_ID_VP9 => Some("VP9"),
        AVCodecID_AV_CODEC_ID_AV1 => Some("AV1"),
        _ => None,
    }
}

pub fn audio_codec(id: AVCodecID) -> Option<&'static str> {
    match id {
        AVCodecID_AV_CODEC_ID_AC3 => Some("AC3"),
        AVCodecID_AV_CODEC_ID_EAC3 => Some("AC3 PLUS"),
        AVCodecID_AV_CODEC_ID_AAC => Some("AAC"),
        AVCodecID_AV_CODEC_ID_DTS => Some("DTS"),
        _ => None,
    }
}

/// The BUFFERSTREAM Load payload. The sink envelope matters on webOS 10: the pipeline allocates its
/// decoder against the DECLARED max size/rate, and this TV's table allows H.264 only up to 30 fps at
/// full size - an H.264 load declared as 4K60 is refused with "601 Resource Allocation Error"
/// (plx-native's webOS 10 lab report). So H.264 up to 1080p declares 1080p at its own rate class.
fn payload(d: &Decl, window: &str, pts_to_decode: i64) -> String {
    let (mw, mh, mf) = match d.video {
        "H264" if d.w <= 1920 && d.h <= 1088 => (1920, 1080, fps_class(d.fps)),
        "H264" => (3840, 2160, 30),
        _ => (3840, 2160, 60),
    };
    let mut es = serde_json::json!({"pauseAtDecodeTime": false, "ptsToDecode": pts_to_decode, "seperatedPTS": true});
    if d.fps.0 > 0 && d.fps.1 > 0 { es["videoFpsValue"] = d.fps.0.into(); es["videoFpsScale"] = d.fps.1.into(); }
    let mut codec = serde_json::json!({"video": d.video});
    if let Some(a) = d.audio { codec["audio"] = a.into(); }
    let p = serde_json::json!({"args": [{
        "mediaTransportType": "BUFFERSTREAM",
        "option": {
            "appId": crate::APP_ID,
            "windowId": window,
            "externalStreamingInfo": {
                "contents": {"codec": codec, "esInfo": es, "format": "RAW", "provider": crate::APP_ID},
                "streamQualityInfo": true, "streamQualityInfoNonFlushable": true,
                "audioSync": true, "restartStreaming": false,
                "bufferingCtrInfo": {"bufferMaxLevel": 0, "bufferMinLevel": 0, "preBufferByte": 0,
                    "qBufferLevelAudio": 0, "qBufferLevelVideo": 0,
                    "srcBufferLevelAudio": {"minimum": 1, "maximum": 2 * 1024 * 1024},
                    "srcBufferLevelVideo": {"minimum": 1, "maximum": 24 * 1024 * 1024}}
            },
            "needAudio": d.audio.is_some(),
            "queryPosition": false,
            "lowDelayMode": false,
            "transmission": {"contentsType": "LIVE"},
            "adaptiveStreaming": {"audioOnly": false, "adaptiveResolution": true, "maxWidth": mw, "maxHeight": mh, "maxFrameRate": mf}
        }
    }]});
    p.to_string()
}

fn fps_class(fps: (i32, i32)) -> i32 {
    if fps.0 <= 0 || fps.1 <= 0 { return 60; }
    let f = fps.0 as f64 / fps.1 as f64;
    [24, 25, 30, 50, 60].into_iter().find(|&c| f <= c as f64 + 0.01).unwrap_or(60)
}

/// ADTS header for one raw AAC frame (the pipeline wants ADTS-framed AAC), from the AudioSpecificConfig.
fn adts(asc: &[u8], payload_len: usize) -> [u8; 7] {
    let (mut obj, sr, ch) = if asc.len() >= 2 {
        (asc[0] >> 3, ((asc[0] & 7) << 1) | (asc[1] >> 7), (asc[1] >> 3) & 0xF)
    } else { (2, 4, 2) };
    if obj == 0 || obj > 4 { obj = 2; } // HE-AAC/PS ride in an LC-signalled ADTS stream
    let len = payload_len + 7;
    [0xFF, 0xF1, ((obj - 1) << 6) | (sr << 2) | (ch >> 2), ((ch & 3) << 6) | ((len >> 11) as u8 & 3), ((len >> 3) & 0xFF) as u8, (((len & 7) << 5) as u8) | 0x1F, 0xFC]
}

fn to_ns(ts: i64, tb: AVRational) -> i64 {
    if ts == NOPTS || tb.den == 0 { return NOPTS; }
    (ts as i128 * tb.num as i128 * 1_000_000_000 / tb.den as i128) as i64
}

fn err(code: c_int) -> String {
    let mut b = [0 as c_char; 128];
    unsafe { av_strerror(code, b.as_mut_ptr(), b.len()); CStr::from_ptr(b.as_ptr()).to_string_lossy().into_owned() }
}

struct Unit { es: c_int, pts: i64, data: Vec<u8> }

pub fn spawn(req: Request, agent: ureq::Agent, shared: Arc<Shared>, obj: Arc<Obj>) {
    std::thread::Builder::new().name("engine".into()).spawn(move || {
        let t0 = Instant::now();
        shared.offset_ns.store(req.offset_ns, Ordering::Relaxed);
        shared.audio_lead_ns.store(req.audio_lead_ns, Ordering::Relaxed);
        if let Err(e) = run(&req, agent, &shared, &obj, t0) { shared.fail(e); }
        // teardown: Unload, wait for the pipeline to confirm, then destroy the object
        let o = obj.0.swap(ptr::null_mut(), Ordering::AcqRel);
        if !o.is_null() {
            unsafe { smp::smp_unload(o); }
            let until = Instant::now() + Duration::from_secs(4);
            while !shared.unloaded.load(Ordering::Acquire) && Instant::now() < until { std::thread::sleep(Duration::from_millis(10)); }
            log!("player: unloaded (confirmed={}) after {} ms", shared.unloaded.load(Ordering::Relaxed), t0.elapsed().as_millis());
            unsafe { smp::smp_destroy(o); }
        }
        if !matches!(shared.phase(), Phase::Failed | Phase::Ended) { shared.set_phase(Phase::Stopped); }
        shared.done.store(true, Ordering::Release);
    }).expect("engine thread");
}

fn run(req: &Request, agent: ureq::Agent, sh: &Arc<Shared>, obj: &Arc<Obj>, t0: Instant) -> Result<(), String> {
    let stop = sh.stop.clone();
    sh.set_phase(Phase::Opening);
    let src = HttpSource::open(agent, &req.url, &req.auth, req.seekable, stop.clone())?;
    log!("player: http open {} ms (len={:?})", t0.elapsed().as_millis(), src.len);
    let io = Box::into_raw(Box::new(Io { src }));
    let result = unsafe { demux_and_feed(req, sh, obj, io, t0) };
    unsafe { drop(Box::from_raw(io)); }
    result
}

unsafe fn demux_and_feed(req: &Request, sh: &Arc<Shared>, obj: &Arc<Obj>, io: *mut Io, t0: Instant) -> Result<(), String> {
    let stop = sh.stop.clone();
    const BUF: usize = 256 * 1024;
    let iobuf = av_malloc(BUF) as *mut u8;
    let mut avio = avio_alloc_context(iobuf, BUF as c_int, 0, io as *mut c_void, Some(io_read), None, Some(io_seek));
    if avio.is_null() { return Err("avio alloc".into()); }
    (*avio).seekable = if req.seekable { 1 } else { 0 };
    let mut fmt = avformat_alloc_context();
    (*fmt).pb = avio;
    (*fmt).flags |= 0x0080; // AVFMT_FLAG_CUSTOM_IO
    let mut opts: *mut AVDictionary = ptr::null_mut();
    let k = |s: &str| CString::new(s).unwrap();
    av_dict_set(&mut opts, k("probesize").as_ptr(), k("4000000").as_ptr(), 0);
    av_dict_set(&mut opts, k("analyzeduration").as_ptr(), k("1500000").as_ptr(), 0);
    let empty = k("");
    let r = avformat_open_input(&mut fmt, empty.as_ptr(), ptr::null(), &mut opts);
    av_dict_free(&mut opts);
    let cleanup = |fmt: &mut *mut AVFormatContext, avio: &mut *mut AVIOContext| {
        if !fmt.is_null() { avformat_close_input(fmt); }
        if !avio.is_null() { av_freep(&mut (**avio).buffer as *mut *mut u8 as *mut c_void); avio_context_free(avio); }
    };
    if r < 0 { cleanup(&mut fmt, &mut avio); return Err(format!("open: {}", err(r))); }
    // Matroska/MP4 headers already describe every stream; a short probe fills in the frame rate.
    let r = avformat_find_stream_info(fmt, ptr::null_mut());
    if r < 0 { log!("player: find_stream_info: {}", err(r)); }
    log!("player: demuxer ready {} ms", t0.elapsed().as_millis());

    let res = feed_loop(req, sh, obj, fmt, io, t0, &stop);
    cleanup(&mut fmt, &mut avio);
    res
}

unsafe fn feed_loop(req: &Request, sh: &Arc<Shared>, obj: &Arc<Obj>, fmt: *mut AVFormatContext, io: *mut Io, t0: Instant, stop: &Arc<AtomicBool>) -> Result<(), String> {
    let n = (*fmt).nb_streams as usize;
    let streams = std::slice::from_raw_parts((*fmt).streams, n);
    let pick = |want: Option<i32>, ty: AVMediaType| -> Option<usize> {
        if let Some(i) = want { if (i as usize) < n && (*(*streams[i as usize]).codecpar).codec_type == ty { return Some(i as usize); } }
        let i = av_find_best_stream(fmt, ty, -1, -1, ptr::null_mut(), 0);
        if i >= 0 { Some(i as usize) } else { None }
    };
    let vi = pick(req.video_stream, AVMediaType_AVMEDIA_TYPE_VIDEO).ok_or("no video stream")?;
    let ai = pick(req.audio_stream, AVMediaType_AVMEDIA_TYPE_AUDIO);
    let vst = streams[vi];
    let vpar = (*vst).codecpar;
    let vname = video_codec((*vpar).codec_id).ok_or_else(|| format!("video codec {} not supported by the TV pipeline", (*vpar).codec_id))?;
    // audio the pipeline takes as is goes straight through; anything else is decoded and re-encoded to AC3 here
    let mut reenc: Option<super::audio::Reencoder> = None;
    let mut aname = ai.and_then(|i| audio_codec((*(*streams[i]).codecpar).codec_id));
    if let (Some(i), None) = (ai, aname) {
        let st = streams[i];
        reenc = super::audio::Reencoder::open((*st).codecpar, (*st).time_base);
        match &reenc {
            Some(r) => { log!("player: audio {}", r.desc); aname = Some("AC3"); }
            None => return Err(format!("audio codec {} can't be decoded on the TV", (*(*st).codecpar).codec_id)),
        }
    }
    let fr = if (*vst).avg_frame_rate.num > 0 { (*vst).avg_frame_rate } else { (*vst).r_frame_rate };
    let decl = Decl { video: vname, audio: aname, w: (*vpar).width, h: (*vpar).height, fps: (fr.num, fr.den) };
    sh.video_w.store((*vpar).width as u32, Ordering::Relaxed);
    sh.video_h.store((*vpar).height as u32, Ordering::Relaxed);
    if (*fmt).duration > 0 { sh.duration_ns.store((*fmt).duration * 1000 + req.offset_ns, Ordering::Relaxed); }
    let adesc = reenc.as_ref().map(|r| r.desc.clone()).or(aname.map(String::from)).unwrap_or_else(|| "no audio".into());
    *sh.info.lock().unwrap() = format!("{} {}x{} {}/{} · {adesc}", vname, decl.w, decl.h, fr.num, fr.den);
    log!("player: v=#{vi} {vname} {}x{} fps={}/{} a={:?} {:?}", decl.w, decl.h, fr.num, fr.den, ai, aname);

    // Annex-B for H.264/HEVC (Matroska/MP4 carry length-prefixed NALs + avcC/hvcC)
    let mut bsf: *mut AVBSFContext = ptr::null_mut();
    let bsf_name = match vname { "H264" => Some("h264_mp4toannexb"), "H265" => Some("hevc_mp4toannexb"), _ => None };
    if let Some(name) = bsf_name {
        let c = CString::new(name).unwrap();
        let f = av_bsf_get_by_name(c.as_ptr());
        if !f.is_null() && av_bsf_alloc(f, &mut bsf) == 0 {
            avcodec_parameters_copy((*bsf).par_in, vpar);
            (*bsf).time_base_in = (*vst).time_base;
            if av_bsf_init(bsf) < 0 { av_bsf_free(&mut bsf); }
        }
    }
    let asc: Vec<u8> = match ai {
        Some(i) if aname == Some("AAC") => {
            let p = (*streams[i]).codecpar;
            if (*p).extradata.is_null() { vec![] } else { std::slice::from_raw_parts((*p).extradata, (*p).extradata_size as usize).to_vec() }
        }
        _ => vec![],
    };

    // image subtitles (PGS / DVD) are decoded right here with the TV's own libavcodec
    let si_sub = req.sub_stream.map(|i| i as usize).filter(|&i| i < n && (*(*streams[i]).codecpar).codec_type == AVMediaType_AVMEDIA_TYPE_SUBTITLE);
    let mut sub_ctx: *mut AVCodecContext = ptr::null_mut();
    if let Some(i) = si_sub {
        let par = (*streams[i]).codecpar;
        let dec = avcodec_find_decoder((*par).codec_id);
        if !dec.is_null() {
            sub_ctx = avcodec_alloc_context3(dec);
            avcodec_parameters_to_context(sub_ctx, par);
            if avcodec_open2(sub_ctx, dec, ptr::null_mut()) < 0 { avcodec_free_context(&mut sub_ctx); }
        }
        log!("player: image subtitles on stream #{i} decoder={}", !sub_ctx.is_null());
    }

    // every embedded text subtitle track is collected as it passes by (tiny packets), so switching tracks is instant
    // and the server never has to read the whole file to extract one (1-4 s on the first request)
    let text_kind = |i: usize| -> Option<(bool, bool)> {
        let par = (*streams[i]).codecpar;
        if (*par).codec_type != AVMediaType_AVMEDIA_TYPE_SUBTITLE || !req.seekable { return None; }
        match (*par).codec_id {
            AVCodecID_AV_CODEC_ID_ASS | AVCodecID_AV_CODEC_ID_SSA => Some((true, false)),
            AVCodecID_AV_CODEC_ID_SUBRIP | AVCodecID_AV_CODEC_ID_SRT | AVCodecID_AV_CODEC_ID_TEXT | AVCodecID_AV_CODEC_ID_WEBVTT => Some((false, false)),
            AVCodecID_AV_CODEC_ID_MOV_TEXT => Some((false, true)),
            _ => None,
        }
    };
    let text_streams: Vec<Option<(bool, bool)>> = (0..n).map(text_kind).collect();
    if text_streams.iter().any(|t| t.is_some()) { log!("player: text subtitles demuxed on the TV: {} tracks", text_streams.iter().filter(|t| t.is_some()).count()); }

    // seek to the start position (keyframe at or before it)
    let local_start = req.start_ns - req.offset_ns;
    if req.seekable && local_start > 1_000_000_000 {
        let r = av_seek_frame(fmt, -1, local_start / 1000, 1 /* AVSEEK_FLAG_BACKWARD */);
        log!("player: seek to {} ms rv={r}", local_start / 1_000_000);
    }

    let pkt = av_packet_alloc();
    let mut queue: VecDeque<Unit> = VecDeque::new();
    let mut first_v: Option<i64> = None;
    let mut eof = false;

    // set after an in-place seek: the bitstream filter and the audio re-encoder start over
    let reset = std::cell::Cell::new(false);

    // returns false at end of file
    let mut read_one = |queue: &mut VecDeque<Unit>, first_v: &mut Option<i64>| -> Result<bool, String> {
        if reset.replace(false) {
            if !bsf.is_null() { av_bsf_flush(bsf); }
            if let (true, Some(i)) = (reenc.is_some(), ai) { reenc = super::audio::Reencoder::open((*streams[i]).codecpar, (*streams[i]).time_base); }
        }
        let r = av_read_frame(fmt, pkt);
        if r < 0 {
            if stop.load(Ordering::Relaxed) { return Err("stopped".into()); }
            if r == AVERROR_EOF { return Ok(false); }
            return Err(format!("read: {}", err(r)));
        }
        let si = (*pkt).stream_index as usize;
        if si == vi {
            let key = (*pkt).flags & AV_PKT_FLAG_KEY != 0;
            if first_v.is_none() && !key { av_packet_unref(pkt); return Ok(true); }
            let tb = (*vst).time_base;
            let emit = |p: *mut AVPacket, queue: &mut VecDeque<Unit>, first_v: &mut Option<i64>| {
                let mut pts = to_ns((*p).pts, tb);
                if pts == NOPTS { pts = to_ns((*p).dts, tb); }
                if pts == NOPTS { return; }
                if first_v.is_none() { *first_v = Some(pts); }
                queue.push_back(Unit { es: ES_VIDEO, pts, data: std::slice::from_raw_parts((*p).data, (*p).size as usize).to_vec() });
            };
            if bsf.is_null() { emit(pkt, queue, first_v); av_packet_unref(pkt); }
            else {
                if av_bsf_send_packet(bsf, pkt) == 0 {
                    loop {
                        let rr = av_bsf_receive_packet(bsf, pkt);
                        if rr == AVERROR_EAGAIN || rr < 0 { break; }
                        emit(pkt, queue, first_v);
                        av_packet_unref(pkt);
                    }
                } else { av_packet_unref(pkt); }
            }
        } else if Some(si) == ai {
            let tb = (*streams[si]).time_base;
            let pts = to_ns((*pkt).pts, tb);
            // audio before the first picture of this session would only be thrown away by the pipeline
            if let (Some(r), Some(v)) = (reenc.as_mut(), *first_v) {
                let mut frames = vec![];
                r.push(pkt, v - 50_000_000, &mut frames);
                for (pts, data) in frames { queue.push_back(Unit { es: ES_AUDIO, pts, data }); }
            } else if reenc.is_none() && pts != NOPTS && first_v.map_or(false, |v| pts >= v - 50_000_000) {
                let raw = std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize);
                let data = if aname == Some("AAC") && !(raw.len() > 1 && raw[0] == 0xFF && raw[1] & 0xF0 == 0xF0) {
                    let mut d = adts(&asc, raw.len()).to_vec(); d.extend_from_slice(raw); d
                } else { raw.to_vec() };
                queue.push_back(Unit { es: ES_AUDIO, pts, data });
            }
            av_packet_unref(pkt);
        } else if let Some(Some((ass, tx3g))) = text_streams.get(si) {
            let tb = (*streams[si]).time_base;
            let start = to_ns((*pkt).pts, tb);
            if start != NOPTS && (*pkt).size > 0 {
                let mut raw = std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize);
                if *tx3g { let l = if raw.len() >= 2 { (raw[0] as usize) << 8 | raw[1] as usize } else { 0 }; raw = &raw[2.min(raw.len())..(2 + l).min(raw.len())]; }
                let dur = to_ns((*pkt).duration, tb);
                let start = start + sh.offset_ns.load(Ordering::Relaxed);
                let end = if dur != NOPTS && dur > 0 { start + dur } else { start + 4_000_000_000 };
                let text = String::from_utf8_lossy(raw).into_owned();
                sh.texts.lock().unwrap().push(TextEvent { stream: si as i32, start_ns: start, end_ns: end, text, ass: *ass });
            }
            av_packet_unref(pkt);
        } else if Some(si) == si_sub && !sub_ctx.is_null() {
            let tb = (*streams[si]).time_base;
            let mut sub: AVSubtitle = std::mem::zeroed();
            let mut got: c_int = 0;
            if avcodec_decode_subtitle2(sub_ctx, &mut sub, &mut got, pkt) >= 0 && got != 0 {
                let base = to_ns((*pkt).pts, tb);
                if base != NOPTS {
                    let start = base + sub.start_display_time as i64 * 1_000_000 + sh.offset_ns.load(Ordering::Relaxed);
                    let end = (sub.end_display_time > sub.start_display_time && sub.end_display_time != u32::MAX)
                        .then(|| start + (sub.end_display_time - sub.start_display_time) as i64 * 1_000_000);
                    let mut rects = vec![];
                    for k in 0..sub.num_rects as usize {
                        let r = &**sub.rects.add(k);
                        if r.w <= 0 || r.h <= 0 || r.data[0].is_null() || r.data[1].is_null() { continue; }
                        let pal = std::slice::from_raw_parts(r.data[1] as *const u32, 256);
                        let mut rgba = Vec::with_capacity((r.w * r.h * 4) as usize);
                        for y in 0..r.h {
                            let row = std::slice::from_raw_parts(r.data[0].add((y * r.linesize[0]) as usize), r.w as usize);
                            for &ix in row { let c = pal[ix as usize]; rgba.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8, (c >> 24) as u8]); }
                        }
                        rects.push(SubRect { x: r.x, y: r.y, w: r.w, h: r.h, rgba });
                    }
                    let canvas = ((*sub_ctx).width.max(1), (*sub_ctx).height.max(1));
                    let mut q = sh.subs.lock().unwrap();
                    if q.len() > 200 { q.pop_front(); }
                    q.push_back(SubEvent { start_ns: start, end_ns: end, canvas, rects });
                }
                avsubtitle_free(&mut sub);
            }
            av_packet_unref(pkt);
        } else { av_packet_unref(pkt); }
        Ok(true)
    };

    // read until the first keyframe so the Load can name the exact PTS it starts decoding at
    while first_v.is_none() {
        if stop.load(Ordering::Relaxed) { return Ok(()); }
        if !read_one(&mut queue, &mut first_v)? { return Err("no picture in stream".into()); }
    }
    let mut start_pts = first_v.unwrap();
    // a server-converted stream may keep the original timestamps (then no offset) or start at zero
    if !req.seekable && start_pts > 5_000_000_000 { sh.offset_ns.store(0, Ordering::Relaxed); }
    let off = sh.offset_ns.load(Ordering::Relaxed);
    sh.position_ns.store(start_pts + off, Ordering::Relaxed);
    log!("player: first keyframe at {} ms after {} ms", start_pts / 1_000_000, t0.elapsed().as_millis());

    let mut fed = [0u64; 3];
    // where the feed thread spends its time: reading/demuxing vs waiting for the pipeline (BufferFull)
    let (mut t_read, mut t_full) = (Duration::ZERO, Duration::ZERO);
    let t_feed = Instant::now();
    let mut last_stat = Instant::now();
    let mut t_seek: Option<Instant> = None;
    let mut seek_target = local_start;
    'pipeline: loop {
        // ---- Load
        sh.set_phase(Phase::Loading);
        let o = smp::smp_create();
        if o.is_null() { return Err("pipeline object".into()); }
        obj.0.store(o, Ordering::Release);
        // Exact positioning: the pipeline decodes from the keyframe but shows frames only from ptsToDecode on (it
        // must not be before the keyframe - that freezes it). It costs ~0.1 s per second skipped, so a keyframe
        // close enough to the target is simply shown.
        let show_from = if req.seekable && seek_target - start_pts > PRECISE_SEEK_NS { seek_target } else { start_pts };
        if show_from != start_pts { log!("player: decoding {} ms ahead to the exact position", (show_from - start_pts) / 1_000_000); }
        let p = payload(&decl, &req.window_id, show_from);
        if t_seek.is_none() { log!("player: Load {}", p); }
        let pl = CString::new(p).unwrap();
        let ctx = Arc::as_ptr(sh) as *mut c_void;
        let ok = smp::smp_load(o, pl.as_ptr(), on_event, ctx);
        log!("player: Load returned {ok} after {} ms", t_seek.unwrap_or(t0).elapsed().as_millis());
        if ok == 0 { return Err("the TV refused to load the stream".into()); }
        let until = Instant::now() + Duration::from_secs(15);
        while !sh.loaded.load(Ordering::Acquire) {
            if stop.load(Ordering::Relaxed) || sh.phase() == Phase::Failed { return Ok(()); }
            if Instant::now() > until { return Err("the TV pipeline did not finish loading".into()); }
            std::thread::sleep(Duration::from_millis(5));
        }
        log!("player: loadCompleted after {} ms", t_seek.unwrap_or(t0).elapsed().as_millis());
        if !sh.paused_req.load(Ordering::Relaxed) { smp::smp_play(o); }
        // lip sync: audio stamped earlier is played earlier, so after the headphones' delay it meets its picture.
        // Audio that would land before 0 is dropped (it is from before the first picture anyway).
        let lead = sh.audio_lead_ns.load(Ordering::Relaxed);
        if lead != 0 { log!("player: audio {} ms ahead of the picture (Bluetooth lip sync)", lead / 1_000_000); }

        // ---- feed: everything queued, then keep reading. BufferFull = the pipeline is ahead; wait.
        let target = loop {
            if last_stat.elapsed() > Duration::from_secs(10) {
                last_stat = Instant::now();
                let el = t_feed.elapsed().as_secs_f64();
                log!("player: feed stats {:.0}s: read {:.0}% wait-full {:.0}% bytes {} MB ({:.1} Mbit/s avg), pos {} ms",
                    el, t_read.as_secs_f64() / el * 100.0, t_full.as_secs_f64() / el * 100.0, (*io).src.bytes / 1_000_000,
                    (*io).src.bytes as f64 * 8.0 / el / 1e6, sh.position_ns.load(Ordering::Relaxed) / 1_000_000);
            }
            if stop.load(Ordering::Relaxed) { break 'pipeline; }
            let t = sh.seek_to.swap(NOPTS, Ordering::AcqRel);
            if t != NOPTS { break t; }
            if queue.is_empty() && !eof {
                let tr = Instant::now();
                let got = read_one(&mut queue, &mut first_v);
                t_read += tr.elapsed();
                match got { Ok(true) => {}, Ok(false) => { eof = true; smp::smp_push_eos(o); log!("player: EOF, pushEOS"); } Err(e) => { if stop.load(Ordering::Relaxed) { break 'pipeline; } return Err(e); } }
                continue;
            }
            if queue.is_empty() && eof {
                if sh.eos.load(Ordering::Acquire) { sh.set_phase(Phase::Ended); }
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            let u = queue.front().unwrap();
            let pts = if u.es == ES_AUDIO { u.pts - lead } else { u.pts };
            if pts < 0 { queue.pop_front(); continue; }
            match smp::smp_feed(o, u.data.as_ptr(), u.data.len() as u32, pts, u.es) as u8 {
                b'O' => { fed[u.es as usize] += 1; queue.pop_front(); }
                b'B' => { let tw = Instant::now(); std::thread::sleep(Duration::from_millis(8)); t_full += tw.elapsed(); }
                _ => { log!("player: feed error at pts {}", u.pts); queue.pop_front(); }
            }
            if fed[1] == 1 && fed[2] <= 1 { log!("player: feeding ({} ms)", t_seek.unwrap_or(t0).elapsed().as_millis()); }
        };

        // ---- in-place seek: the same connection and demuxer, a new pipeline (its own seek needs private struct offsets).
        // The old pipeline unloads while the demuxer already seeks and reads up to the keyframe.
        let ts = Instant::now();
        t_seek = Some(ts);
        // Unload blocks for ~300 ms; let a helper thread wait for it while this one seeks the demuxer
        let addr = o as usize;
        std::thread::Builder::new().name("unload".into()).spawn(move || unsafe { smp::smp_unload(addr as *mut c_void); }).ok();
        queue.clear();
        first_v = None;
        eof = false;
        fed = [0; 3];
        reset.set(true);
        let local = target - sh.offset_ns.load(Ordering::Relaxed);
        seek_target = local;
        let r = av_seek_frame(fmt, -1, local.max(0) / 1000, 1 /* AVSEEK_FLAG_BACKWARD */);
        while first_v.is_none() {
            if stop.load(Ordering::Relaxed) { break 'pipeline; }
            if !read_one(&mut queue, &mut first_v)? { return Err("no picture after the seek".into()); }
        }
        start_pts = first_v.unwrap();
        log!("player: seek to {} ms rv={r}: keyframe {} ms after {} ms", local / 1_000_000, start_pts / 1_000_000, ts.elapsed().as_millis());
        let until = Instant::now() + Duration::from_secs(4);
        while !sh.unloaded.load(Ordering::Acquire) && Instant::now() < until { std::thread::sleep(Duration::from_millis(5)); }
        log!("player: old pipeline unloaded (confirmed={}) after {} ms", sh.unloaded.load(Ordering::Relaxed), ts.elapsed().as_millis());
        obj.0.store(ptr::null_mut(), Ordering::Release);
        smp::smp_destroy(o);
        sh.loaded.store(false, Ordering::Release);
        sh.unloaded.store(false, Ordering::Release);
        sh.eos.store(false, Ordering::Release);
        if sh.phase() == Phase::Ended { sh.set_phase(Phase::Loading); }
        sh.seeking.store(false, Ordering::Release); // the old pipeline is gone: events are the new one's again
    }
    av_packet_free(&mut (pkt as *mut AVPacket));
    if !bsf.is_null() { av_bsf_free(&mut bsf); }
    if !sub_ctx.is_null() { avcodec_free_context(&mut sub_ctx); }
    log!("player: fed video={} audio={}", fed[1], fed[2]);
    Ok(())
}
