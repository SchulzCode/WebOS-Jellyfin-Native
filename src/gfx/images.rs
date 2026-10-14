//! Remote images (Jellyfin posters, thumbs, backdrops): fetched + decoded on the pool, uploaded on the UI
//! thread a few per frame, kept in an LRU within a GPU memory budget, faded in when they arrive.
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
                let resp = agent.get(&url).call().ok()?;
                let mut buf = Vec::with_capacity(256 * 1024);
                std::io::Read::read_to_end(&mut resp.into_reader(), &mut buf).ok()?;
                let img = image::load_from_memory(&buf).ok()?.to_rgba8();
                Some((img.width(), img.height(), img.into_raw()))
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
