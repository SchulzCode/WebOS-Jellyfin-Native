//! Audio the TV's decoder doesn't take (TrueHD, FLAC, ALAC, Opus, Vorbis, PCM, ...) is decoded here with the
//! TV's own libavcodec and re-encoded to AC3, which the hardware plays natively: 5.1 at 640 kb/s (7.1 is
//! folded into 5.1) or stereo at 256 kb/s. LG's BUFFERSTREAM only accepts PCM as 16-bit stereo
//! (libpf's LUT_ChannelModeType/LUT_BPSType), so AC3 is how surround survives. Costs a few percent of a core.
use crate::sys::ff::*;
use std::ffi::{c_int, CStr, CString};
use std::ptr;

const NOPTS: i64 = i64::MIN;
const AC3_RATES: [c_int; 3] = [48000, 44100, 32000];

pub struct Reencoder {
    dec: *mut AVCodecContext,
    enc: *mut AVCodecContext,
    swr: *mut SwrContext,
    frame: *mut AVFrame,
    pkt: *mut AVPacket,
    /// converted samples waiting for a full AC3 frame, one Vec per output channel
    fifo: Vec<Vec<f32>>,
    tb: AVRational,
    /// item-stream time of the first sample we keep
    base_ns: Option<i64>,
    emitted: i64,
    pub desc: String,
}

unsafe impl Send for Reencoder {}

fn to_ns(ts: i64, tb: AVRational) -> i64 {
    if ts == NOPTS || tb.den == 0 { return NOPTS; }
    (ts as i128 * tb.num as i128 * 1_000_000_000 / tb.den as i128) as i64
}

impl Reencoder {
    /// None if the TV's libavcodec has no decoder for the stream (or the encoder can't be set up).
    pub unsafe fn open(par: *const AVCodecParameters, tb: AVRational) -> Option<Reencoder> {
        let codec = avcodec_find_decoder((*par).codec_id);
        if codec.is_null() { return None; }
        let mut dec = avcodec_alloc_context3(codec);
        avcodec_parameters_to_context(dec, par);
        (*dec).pkt_timebase = tb;
        if avcodec_open2(dec, codec, ptr::null_mut()) < 0 { avcodec_free_context(&mut dec); return None; }

        let ch = (*par).channels.max(1);
        let layout = match ch { 1 => AV_CH_LAYOUT_MONO, 2 => AV_CH_LAYOUT_STEREO, _ => AV_CH_LAYOUT_5POINT1 } as u64;
        let rate = if AC3_RATES.contains(&(*par).sample_rate) { (*par).sample_rate } else { 48000 };
        let name = CString::new("ac3").unwrap();
        let ecodec = avcodec_find_encoder_by_name(name.as_ptr());
        if ecodec.is_null() { avcodec_free_context(&mut dec); return None; }
        let mut enc = avcodec_alloc_context3(ecodec);
        (*enc).sample_fmt = AVSampleFormat_AV_SAMPLE_FMT_FLTP;
        (*enc).sample_rate = rate;
        (*enc).channel_layout = layout;
        (*enc).channels = av_get_channel_layout_nb_channels(layout);
        (*enc).bit_rate = if (*enc).channels >= 6 { 640_000 } else { 256_000 };
        (*enc).time_base = AVRational { num: 1, den: rate };
        if avcodec_open2(enc, ecodec, ptr::null_mut()) < 0 {
            avcodec_free_context(&mut enc); avcodec_free_context(&mut dec); return None;
        }
        let src = CStr::from_ptr((*codec).name).to_string_lossy().to_uppercase();
        let desc = format!("{src} {ch}ch → AC3 {}ch (TV)", (*enc).channels);
        Some(Reencoder {
            dec, enc, swr: ptr::null_mut(), frame: av_frame_alloc(), pkt: av_packet_alloc(),
            fifo: vec![Vec::new(); (*enc).channels as usize], tb, base_ns: None, emitted: 0, desc,
        })
    }

    /// Decode one demuxed packet and append the finished AC3 frames to `out` as (pts in ns, bytes).
    /// Audio before `min_ns` is dropped (the pipeline would throw it away anyway).
    pub unsafe fn push(&mut self, pkt: *const AVPacket, min_ns: i64, out: &mut Vec<(i64, Vec<u8>)>) {
        if avcodec_send_packet(self.dec, pkt) < 0 { return; }
        let f = self.frame;
        while avcodec_receive_frame(self.dec, f) == 0 {
            if self.base_ns.is_none() {
                let ts = if (*f).best_effort_timestamp != NOPTS { (*f).best_effort_timestamp } else { (*f).pts };
                let ns = to_ns(ts, self.tb);
                if ns == NOPTS || ns < min_ns { av_frame_unref(f); continue; }
                self.base_ns = Some(ns);
            }
            if self.swr.is_null() && !self.init_swr(f) { av_frame_unref(f); continue; }
            let n = swr_get_out_samples(self.swr, (*f).nb_samples).max(0) as usize;
            let mut bufs: Vec<Vec<f32>> = vec![vec![0.0; n]; self.fifo.len()];
            let mut ptrs: Vec<*mut u8> = bufs.iter_mut().map(|b| b.as_mut_ptr() as *mut u8).collect();
            let got = swr_convert(self.swr, ptrs.as_mut_ptr(), n as c_int, (*f).extended_data as *mut *const u8, (*f).nb_samples);
            av_frame_unref(f);
            if got <= 0 { continue; }
            for (q, b) in self.fifo.iter_mut().zip(&bufs) { q.extend_from_slice(&b[..got as usize]); }
            self.encode(out);
        }
    }

    unsafe fn init_swr(&mut self, f: *const AVFrame) -> bool {
        let in_layout = if (*f).channel_layout != 0 { (*f).channel_layout as i64 } else { av_get_default_channel_layout((*f).channels) };
        self.swr = swr_alloc_set_opts(ptr::null_mut(), (*self.enc).channel_layout as i64, (*self.enc).sample_fmt, (*self.enc).sample_rate,
            in_layout, (*f).format, (*f).sample_rate, 0, ptr::null_mut());
        !self.swr.is_null() && swr_init(self.swr) >= 0
    }

    unsafe fn encode(&mut self, out: &mut Vec<(i64, Vec<u8>)>) {
        let fs = (*self.enc).frame_size.max(1) as usize;
        let rate = (*self.enc).sample_rate as i64;
        let base = self.base_ns.unwrap_or(0);
        while self.fifo[0].len() >= fs {
            let mut o = av_frame_alloc();
            (*o).nb_samples = fs as c_int;
            (*o).format = (*self.enc).sample_fmt;
            (*o).channel_layout = (*self.enc).channel_layout;
            (*o).channels = (*self.enc).channels;
            (*o).sample_rate = (*self.enc).sample_rate;
            if av_frame_get_buffer(o, 0) < 0 { av_frame_free(&mut o); return; }
            for (c, q) in self.fifo.iter_mut().enumerate() {
                std::ptr::copy_nonoverlapping(q.as_ptr(), (*o).data[c] as *mut f32, fs);
                q.drain(..fs);
            }
            (*o).pts = self.emitted;
            self.emitted += fs as i64;
            let r = avcodec_send_frame(self.enc, o);
            av_frame_free(&mut o);
            if r < 0 { return; }
            while avcodec_receive_packet(self.enc, self.pkt) == 0 {
                let p = self.pkt;
                let data = std::slice::from_raw_parts((*p).data, (*p).size as usize).to_vec();
                out.push((base + (*p).pts * 1_000_000_000 / rate, data));
                av_packet_unref(p);
            }
        }
    }
}

impl Drop for Reencoder {
    fn drop(&mut self) {
        unsafe {
            if !self.swr.is_null() { swr_free(&mut self.swr); }
            av_frame_free(&mut self.frame);
            av_packet_free(&mut self.pkt);
            avcodec_free_context(&mut self.enc);
            avcodec_free_context(&mut self.dec);
        }
    }
}
