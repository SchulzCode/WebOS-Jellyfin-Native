//! Remote images (Jellyfin posters, thumbs, backdrops): fetched + decoded on the pool, uploaded on the UI
//! thread a few per frame, kept in an LRU within a GPU memory budget, faded in when they arrive.
//! A `gray:` prefix on the address gives a greyed-out copy (download cards: what isn't there yet).
use super::{Gfx, Texture};
use crate::net::{self, Pool};
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Instant;

enum Entry { Loading, Ready { tex: Texture, used: u64, at: Instant }, Failed }

type Decoded = (String, Option<(u32, u32, Vec<u8>)>);

pub struct Images {
    map: HashMap<String, Entry>,
    tx: Sender<Decoded>,
    rx: Receiver<Decoded>,
    pool: Pool,
    agent: ureq::Agent,
    queue: Vec<Decoded>,
    frame: u64,
    bytes: usize,
    budget: usize,
}

const GRAY: &str = "gray:";

/// Decode to RGBA. Baseline JPEGs that store each colour in its own scan (Sonarr/Radarr write their resized pictures
/// like that) come out of image 0.25's decoder (zune-jpeg 0.5.15) as green stripes, without an error - those go to
/// jpeg-decoder instead.
fn decode(buf: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if split_scan_jpeg(buf) {
        let mut d = jpeg_decoder::Decoder::new(std::io::Cursor::new(buf));
        if let (Ok(px), Some(info)) = (d.decode(), d.info()) {
            let rgba: Option<Vec<u8>> = match info.pixel_format {
                jpeg_decoder::PixelFormat::RGB24 => Some(px.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect()),
                jpeg_decoder::PixelFormat::L8 => Some(px.iter().flat_map(|&l| [l, l, l, 255]).collect()),
                _ => None, // CMYK etc.: let image try
            };
            if let Some(rgba) = rgba { return Some((info.width as u32, info.height as u32, rgba)); }
        }
    }
    let img = image::load_from_memory(buf).ok()?.to_rgba8();
    Some((img.width(), img.height(), img.into_raw()))
}

/// A baseline JPEG whose first scan carries fewer components than the frame has (non-interleaved).
fn split_scan_jpeg(d: &[u8]) -> bool {
    if d.len() < 4 || d[0] != 0xFF || d[1] != 0xD8 { return false; }
    let (mut i, mut frame) = (2usize, None);
    while i + 4 <= d.len() && d[i] == 0xFF {
        if d[i + 1] == 0xFF { i += 1; continue; } // fill byte
        let (m, len) = (d[i + 1], u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize);
        match m {
            0xC0 | 0xC1 => frame = d.get(i + 9).copied(),
            0xC2 => return false, // progressive: many scans by design, decoded fine
            0xDA => return matches!((frame, d.get(i + 4)), (Some(f), Some(&s)) if s < f),
            _ => {}
        }
        i += 2 + len;
    }
    false
}

/// The `gray:` variant of an image address.
pub fn gray(url: &str) -> String { if url.is_empty() { String::new() } else { format!("{GRAY}{url}") } }

/// Greyscale, a little darker and flatter: reads as "not here yet" next to the colour version.
fn desaturate(px: &mut [u8]) {
    for p in px.chunks_exact_mut(4) {
        let l = 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
        let v = (l * 0.72 + 14.0).min(255.0) as u8;
        p[0] = v; p[1] = v; p[2] = v;
    }
}

impl Images {
    pub fn new(agent: ureq::Agent) -> Images {
        let (tx, rx) = channel();
        Images { map: HashMap::new(), tx, rx, pool: Pool::new(3, "img"), agent, queue: vec![], frame: 0, bytes: 0, budget: 110 << 20 }
    }

    /// The texture for `url` and its fade-in opacity; starts loading it if needed.
    pub fn get(&mut self, url: &str) -> Option<(&Texture, f32)> {
        if url.is_empty() { return None; }
        if !self.map.contains_key(url) { self.request(url); return None; }
        match self.map.get_mut(url) {
            Some(Entry::Ready { tex, used, at }) => {
                *used = self.frame;
                let a = (at.elapsed().as_secs_f32() / 0.28).min(1.0);
                Some((tex, a * a * (3.0 - 2.0 * a)))
            }
            _ => None,
        }
    }

    pub fn is_fading(&self) -> bool {
        self.map.values().any(|e| matches!(e, Entry::Ready { at, used, .. } if *used + 2 >= self.frame && at.elapsed().as_millis() < 300))
    }

    /// Warm the cache without drawing.
    pub fn prefetch(&mut self, url: &str) { if !url.is_empty() && !self.map.contains_key(url) { self.request(url); } }

    fn request(&mut self, url: &str) {
        self.map.insert(url.to_string(), Entry::Loading);
        let (tx, agent, url) = (self.tx.clone(), self.agent.clone(), url.to_string());
        self.pool.spawn(move || {
            let res = (|| {
                let (src, gray) = match url.strip_prefix(GRAY) { Some(u) => (u, true), None => (url.as_str(), false) };
                let resp = agent.get(src).call().ok()?;
                let mut buf = Vec::with_capacity(256 * 1024);
                std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf).ok()?;
                let (w, h, mut px) = decode(&buf)?;
                if gray { desaturate(&mut px); }
                Some((w, h, px))
            })();
            if res.is_none() { crate::log!("image failed: {url}"); }
            let _ = tx.send((url, res));
            net::wake();
        });
    }

    /// Upload finished images (bounded per frame) and evict the least recently used over budget.
    /// Returns true when something new became visible.
    pub fn pump(&mut self, g: &Gfx) -> bool {
        self.frame += 1;
        self.queue.extend(self.rx.try_iter());
        let mut changed = false;
        let mut uploaded_px = 0usize;
        while let Some((url, res)) = self.queue.pop() {
            match res {
                Some((w, h, px)) => {
                    let tex = unsafe { g.upload_rgba(w, h, &px) };
                    self.bytes += (w * h * 4) as usize;
                    uploaded_px += (w * h) as usize;
                    self.map.insert(url, Entry::Ready { tex, used: self.frame, at: Instant::now() });
                }
                None => { self.map.insert(url, Entry::Failed); }
            }
            changed = true;
            if uploaded_px > 2_500_000 { break; } // keep a frame under ~10 ms of uploads
        }
        if self.bytes > self.budget {
            let mut ready: Vec<(u64, String)> = self.map.iter().filter_map(|(k, e)| match e { Entry::Ready { used, .. } => Some((*used, k.clone())), _ => None }).collect();
            ready.sort();
            for (used, k) in ready {
                if self.bytes <= self.budget * 8 / 10 || used + 2 >= self.frame { break; }
                if let Some(Entry::Ready { tex, .. }) = self.map.remove(&k) {
                    self.bytes -= (tex.w * tex.h * 4) as usize;
                    unsafe { g.free(tex); }
                }
            }
        }
        changed || !self.queue.is_empty()
    }
}
