//! HTTP(S) byte source behind FFmpeg's custom I/O: range requests for direct play (seekable), a single
//! streamed response for server transcodes (not seekable). The TV's FFmpeg is built without network
//! protocols, so every byte FFmpeg sees comes through here.
//!
//! Matroska attachments (anime releases carry 100+ subtitle fonts, tens of MB) are never downloaded: FFmpeg's
//! demuxer reads them all while opening, which took 4+ s, and we don't use them. Their byte range is served
//! as one EBML Void element, which the demuxer skips.
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct HttpSource {
    agent: ureq::Agent,
    url: String,
    auth: String,
    pub seekable: bool,
    pub len: Option<u64>,
    pos: u64,
    body: Option<Box<dyn Read + Send>>,
    pub stop: Arc<AtomicBool>,
    pub bytes: u64,
    /// byte range served as a Void element instead of fetched (Matroska attachments)
    hole: Option<(u64, u64)>,
}

impl HttpSource {
    pub fn open(agent: ureq::Agent, url: &str, auth: &str, seekable: bool, stop: Arc<AtomicBool>) -> Result<Self, String> {
        let mut s = HttpSource { agent, url: url.to_string(), auth: auth.to_string(), seekable, len: None, pos: 0, body: None, stop, bytes: 0, hole: None };
        s.connect(0)?;
        if seekable {
            s.hole = s.mkv_attachments();
            if let Some((a, b)) = s.hole { crate::log!("source: skipping {} KB of Matroska attachments", (b - a) / 1024); }
        }
        Ok(s)
    }

    fn connect(&mut self, at: u64) -> Result<(), String> {
        self.body = None;
        let t = std::time::Instant::now();
        let mut req = self.agent.get(&self.url).set("Authorization", &self.auth);
        if self.seekable { req = req.set("Range", &format!("bytes={at}-")); }
        let resp = req.call().map_err(|e| format!("http: {e}"))?;
        if self.seekable {
            // "bytes 0-123/456" -> total length
            if let Some(cr) = resp.header("Content-Range") {
                self.len = cr.rsplit('/').next().and_then(|t| t.trim().parse().ok());
            } else if resp.status() == 200 {
                if at > 0 { return Err("server ignored the range request".into()); }
                self.len = resp.header("Content-Length").and_then(|v| v.parse().ok());
            }
        }
        crate::log!("source: GET at {at} -> {} in {} ms", resp.status(), t.elapsed().as_millis());
        self.pos = at;
        self.body = Some(Box::new(resp.into_reader()));
        Ok(())
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        if self.stop.load(Ordering::Relaxed) { return Err("stopped".into()); }
        let mut buf = buf;
        if let Some((a, b)) = self.hole {
            if self.pos >= a && self.pos < b {
                // inside the hole: a Void element header, then zeros; the connection is dropped and resumes after it
                let n = buf.len().min((b - self.pos) as usize);
                let void = void_header(b - a);
                for (i, x) in buf[..n].iter_mut().enumerate() {
                    let off = (self.pos - a) as usize + i;
                    *x = if off < void.len() { void[off] } else { 0 };
                }
                self.pos += n as u64;
                self.body = None;
                return Ok(n);
            }
            if self.pos < a && self.pos + buf.len() as u64 > a { let n = (a - self.pos) as usize; buf = &mut buf[..n]; }
        }
        if self.body.is_none() { self.connect(self.pos)?; }
        let mut tries = 0;
        loop {
            match self.body.as_mut().unwrap().read(buf) {
                Ok(n) => { self.pos += n as u64; self.bytes += n as u64; return Ok(n); }
                Err(e) => {
                    // a dropped connection mid-file: resume from where we were (direct play only)
                    tries += 1;
                    if !self.seekable || tries > 3 || self.stop.load(Ordering::Relaxed) { return Err(format!("read: {e}")); }
                    crate::log!("source: read error {e}, reconnecting at {}", self.pos);
                    std::thread::sleep(std::time::Duration::from_millis(300 * tries));
                    self.connect(self.pos)?;
                }
            }
        }
    }

    pub fn seek(&mut self, to: u64) -> Result<u64, String> {
        if !self.seekable { return Err("not seekable".into()); }
        if to == self.pos && self.body.is_some() { return Ok(to); }
        // short forward hops are cheaper to read through than to reconnect (MKV cues/clusters)
        if to > self.pos && to - self.pos < 256 * 1024 && self.body.is_some() {
            let mut left = to - self.pos;
            let mut sink = [0u8; 16 * 1024];
            while left > 0 {
                let n = self.read(&mut sink[..left.min(16 * 1024) as usize])?;
                if n == 0 { break; }
                left -= n as u64;
            }
            return Ok(self.pos);
        }
        self.pos = to;
        self.body = None; // reconnect lazily on the next read
        Ok(to)
    }

    /// One ranged GET (no retries; failures just mean no hole).
    fn fetch(&self, from: u64, len: u64) -> Option<Vec<u8>> {
        let t = std::time::Instant::now();
        let resp = self.agent.get(&self.url).set("Authorization", &self.auth).set("Range", &format!("bytes={from}-{}", from + len - 1)).call().ok()?;
        if resp.status() != 206 { return None; }
        let mut v = Vec::with_capacity(len as usize);
        resp.into_reader().take(len).read_to_end(&mut v).ok()?;
        crate::log!("source: fetch {from}+{len} in {} ms", t.elapsed().as_millis());
        Some(v)
    }

    /// The byte range of a Matroska file's Attachments element (EBML header -> Segment -> SeekHead -> Seek).
    fn mkv_attachments(&self) -> Option<(u64, u64)> {
        const SEGMENT: u64 = 0x18538067; const SEEKHEAD: u64 = 0x114D9B74; const SEEK: u64 = 0x4DBB;
        const SEEK_ID: u64 = 0x53AB; const SEEK_POS: u64 = 0x53AC; const ATTACHMENTS: u64 = 0x1941A469;
        let head = self.fetch(0, 64 * 1024)?;
        let mut r = Ebml { b: &head, p: 0 };
        let (id, size) = r.element()?;
        if id != 0x1A45DFA3 { return None; } // not Matroska/WebM
        r.p += size as usize;
        let (id, _) = r.element()?;
        if id != SEGMENT { return None; }
        let seg = r.p as u64;
        // the SeekHead is the Segment's first child in practice; walk a few top-level elements to find it
        let mut at = None;
        for _ in 0..4 {
            let (id, size) = r.element()?;
            let end = r.p + size as usize;
            if id == SEEKHEAD {
                while r.p < end.min(head.len()) {
                    let (id, size) = r.element()?;
                    let e = r.p + size as usize;
                    if id == SEEK {
                        let (mut sid, mut spos) = (0u64, None);
                        while r.p < e {
                            let (cid, cs) = r.element()?;
                            let v = r.uint(cs as usize)?;
                            if cid == SEEK_ID { sid = v; } else if cid == SEEK_POS { spos = Some(v); }
                        }
                        if sid == ATTACHMENTS { at = spos.map(|p| seg + p); }
                    }
                    r.p = e;
                }
                break;
            }
            r.p = end;
            if r.p >= head.len() { break; }
        }
        let at = at?;
        let hdr = self.fetch(at, 16)?;
        let mut h = Ebml { b: &hdr, p: 0 };
        let (id, size) = h.element()?;
        if id != ATTACHMENTS || size < 1024 { return None; }
        let end = at + h.p as u64 + size;
        if self.len.map_or(false, |l| end > l) { return None; }
        Some((at, end))
    }
}

/// EBML Void (ID 0xEC) with an 8-byte size covering `total` bytes, header included.
fn void_header(total: u64) -> [u8; 9] {
    let n = total - 9;
    let mut h = [0xEC, 0x01, 0, 0, 0, 0, 0, 0, 0];
    for i in 0..7 { h[2 + i] = (n >> (8 * (6 - i))) as u8; }
    h
}

struct Ebml<'a> { b: &'a [u8], p: usize }

impl Ebml<'_> {
    /// Variable-length integer; `keep_marker` for element IDs.
    fn vint(&mut self, keep_marker: bool) -> Option<u64> {
        let first = *self.b.get(self.p)?;
        let len = first.leading_zeros() as usize + 1;
        if len > 8 || self.p + len > self.b.len() { return None; }
        let mut v = if keep_marker { first as u64 } else { (first as u64) & (0xFF >> len) };
        for i in 1..len { v = (v << 8) | self.b[self.p + i] as u64; }
        self.p += len;
        Some(v)
    }
    fn element(&mut self) -> Option<(u64, u64)> { let id = self.vint(true)?; let size = self.vint(false)?; Some((id, size)) }
    fn uint(&mut self, n: usize) -> Option<u64> {
        if n > 8 || self.p + n > self.b.len() { return None; }
        let v = self.b[self.p..self.p + n].iter().fold(0u64, |a, &x| (a << 8) | x as u64);
        self.p += n;
        Some(v)
    }
}
