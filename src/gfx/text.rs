//! Text: Inter (static weights) rasterised on demand with fontdue into one 2048x2048 alpha atlas.
//! Glyphs are cached per (weight, char, pixel size); only the dirty band of the atlas is re-uploaded.
use super::{Color, Gfx, Rect};
use glow::HasContext;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum W { Regular = 1, Medium = 2, Semi = 3, Bold = 4 }

impl W { fn ix(self) -> usize { match self { W::Regular => 0, W::Medium => 1, W::Semi => 2, W::Bold => 3 } } }

const ATLAS: usize = 2048;

#[derive(Clone, Copy)]
struct Glyph { x: u16, y: u16, w: u16, h: u16, xmin: f32, ymin: f32, adv: f32 }

pub struct Text {
    fonts: Vec<fontdue::Font>,
    fallback: Vec<(&'static str, Option<Option<fontdue::Font>>)>,
    cache: HashMap<(u8, char, u16), Glyph>,
    pixels: Vec<u8>,
    tex: glow::Texture,
    shelf_x: usize,
    shelf_y: usize,
    shelf_h: usize,
    dirty: Option<(usize, usize)>,
    pub prof_us: u64,
    /// fitted text is laid out once, not every frame: (text, size, weight, max width, lines) -> lines
    fit: HashMap<(String, u16, u8, u32, u8), Vec<String>>,
}

fn load(path: &str) -> Option<fontdue::Font> {
    let bytes = std::fs::read(path).ok()?;
    fontdue::Font::from_bytes(bytes, fontdue::FontSettings { scale: 64.0, ..Default::default() }).ok()
}

impl Text {
    pub fn new(gl: &glow::Context) -> Text {
        let dir = crate::assets_dir();
        // fontdue pre-processes every glyph when it parses a font: do the four weights in parallel
        let names = ["Inter-Regular", "Inter-Medium", "Inter-SemiBold", "Inter-Bold"];
        let t0 = std::time::Instant::now();
        let handles: Vec<_> = names.iter().map(|n| { let p = format!("{dir}/fonts/{n}.ttf"); std::thread::spawn(move || load(&p).expect("font")) }).collect();
        let fonts: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        crate::log!("text: fonts parsed in {} ms", t0.elapsed().as_millis());
        // other scripts from the TV's own fonts (Jellyfin titles are often Japanese); loaded on first use
        let fallback = vec![("/usr/share/fonts/LG_Display_JP.ttf", None), ("/usr/share/fonts/DroidSansFallback.ttf", None), ("/usr/share/fonts/LG_Smart_UI-Regular.ttf", None)];
        let tex = unsafe {
            let t = gl.create_texture().unwrap();
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::ALPHA as i32, ATLAS as i32, ATLAS as i32, 0, glow::ALPHA, glow::UNSIGNED_BYTE, None);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            t
        };
        Text { fonts, fallback, cache: HashMap::new(), pixels: vec![0; ATLAS * ATLAS], tex, shelf_x: 1, shelf_y: 1, shelf_h: 0, dirty: Some((0, ATLAS)), prof_us: 0, fit: HashMap::new() }
    }

    fn font_for(&mut self, w: W, c: char) -> &fontdue::Font {
        if self.fonts[w.ix()].lookup_glyph_index(c) != 0 || c.is_whitespace() || (c as u32) < 0x2E80 && !matches!(c as u32, 0x0370..=0x1FFF) {
            return &self.fonts[w.ix()];
        }
        let mut hit = None;
        for (i, (path, slot)) in self.fallback.iter_mut().enumerate() {
            if slot.is_none() { *slot = Some(load(path)); crate::log!("text: loaded fallback {path}"); }
            if let Some(Some(f)) = slot { if f.lookup_glyph_index(c) != 0 { hit = Some(i); break; } }
        }
        match hit { Some(i) => self.fallback[i].1.as_ref().unwrap().as_ref().unwrap(), None => &self.fonts[w.ix()] }
    }

    fn glyph(&mut self, w: W, c: char, px: u16) -> Glyph {
        if let Some(g) = self.cache.get(&(w as u8, c, px)) { return *g; }
        let (m, bmp) = self.font_for(w, c).rasterize(c, px as f32);
        let (gw, gh) = (m.width, m.height);
        if self.shelf_x + gw + 1 >= ATLAS { self.shelf_x = 1; self.shelf_y += self.shelf_h + 1; self.shelf_h = 0; }
        if self.shelf_y + gh + 1 >= ATLAS {
            // atlas full: start over (glyphs re-rasterise on demand; happens only after huge variety)
            self.cache.clear(); self.pixels.iter_mut().for_each(|p| *p = 0);
            self.shelf_x = 1; self.shelf_y = 1; self.shelf_h = 0; self.dirty = Some((0, ATLAS));
        }
        let (x, y) = (self.shelf_x, self.shelf_y);
        for row in 0..gh { self.pixels[(y + row) * ATLAS + x..(y + row) * ATLAS + x + gw].copy_from_slice(&bmp[row * gw..row * gw + gw]); }
        self.shelf_x += gw + 1;
        self.shelf_h = self.shelf_h.max(gh);
        self.dirty = Some(match self.dirty { Some((a, b)) => (a.min(y), b.max(y + gh)), None => (y, y + gh) });
        let g = Glyph { x: x as u16, y: y as u16, w: gw as u16, h: gh as u16, xmin: m.xmin as f32, ymin: m.ymin as f32, adv: m.advance_width };
        self.cache.insert((w as u8, c, px), g);
        g
    }

    pub fn upload(&mut self, gl: &glow::Context) {
        if let Some((a, b)) = self.dirty.take() {
            let b = b.min(ATLAS);
            unsafe {
                gl.bind_texture(glow::TEXTURE_2D, Some(self.tex));
                gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
                gl.tex_sub_image_2d(glow::TEXTURE_2D, 0, 0, a as i32, ATLAS as i32, (b - a) as i32, glow::ALPHA, glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(&self.pixels[a * ATLAS..b * ATLAS]));
            }
        }
    }

    pub fn ascent(&self, size: f32) -> f32 {
        self.fonts[0].horizontal_line_metrics(size).map(|m| m.ascent).unwrap_or(size * 0.97)
    }

    pub fn measure(&mut self, s: &str, size: f32, w: W, track: f32) -> f32 {
        let px = size.round() as u16;
        let mut x = 0.0;
        let mut prev: Option<char> = None;
        for c in s.chars() {
            if let Some(p) = prev { x += self.fonts[w.ix()].horizontal_kern(p, c, size).unwrap_or(0.0); }
            x += self.glyph(w, c, px).adv + track;
            prev = Some(c);
        }
        if x > 0.0 { x - track } else { 0.0 }
    }

    fn fit_key(s: &str, size: f32, w: W, max: f32, lines: usize) -> (String, u16, u8, u32, u8) {
        (s.to_string(), (size * 4.0) as u16, w as u8, max.to_bits(), lines as u8)
    }
    fn remember(&mut self, k: (String, u16, u8, u32, u8), v: Vec<String>) {
        if self.fit.len() > 4000 { self.fit.clear(); }
        self.fit.insert(k, v);
    }

    /// Cut `s` to fit `max` px, ending in an ellipsis.
    pub fn ellipsize(&mut self, s: &str, size: f32, w: W, max: f32) -> String {
        let t0 = std::time::Instant::now();
        let k = Self::fit_key(s, size, w, max, 0);
        let r = match self.fit.get(&k) {
            Some(v) => v[0].clone(),
            None => { let r = self.ellipsize_(s, size, w, max); self.remember(k, vec![r.clone()]); r }
        };
        self.prof_us += t0.elapsed().as_micros() as u64;
        r
    }
    fn ellipsize_(&mut self, s: &str, size: f32, w: W, max: f32) -> String {
        if self.measure(s, size, w, 0.0) <= max { return s.to_string(); }
        let chars: Vec<char> = s.chars().collect();
        let cut = |n: usize| chars[..n].iter().collect::<String>().trim_end().to_string() + "…";
        // longest prefix that fits, by binary search
        let (mut lo, mut hi) = (0usize, chars.len());
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            if self.measure(&cut(mid), size, w, 0.0) <= max { lo = mid; } else { hi = mid - 1; }
        }
        cut(lo)
    }

    /// Word-wrap into at most `lines` lines; the last one is ellipsised.
    pub fn wrap(&mut self, s: &str, size: f32, w: W, max: f32, lines: usize) -> Vec<String> {
        let t0 = std::time::Instant::now();
        let k = Self::fit_key(s, size, w, max, lines.max(1));
        let r = match self.fit.get(&k) {
            Some(v) => v.clone(),
            None => { let p = self.prof_us; let r = self.wrap_(s, size, w, max, lines); self.prof_us = p; self.remember(k, r.clone()); r }
        };
        self.prof_us += t0.elapsed().as_micros() as u64;
        r
    }
    fn wrap_(&mut self, s: &str, size: f32, w: W, max: f32, lines: usize) -> Vec<String> {
        let mut out = vec![];
        let mut cur = String::new();
        let words: Vec<&str> = s.split_whitespace().collect();
        let mut i = 0;
        while i < words.len() {
            let cand = if cur.is_empty() { words[i].to_string() } else { format!("{cur} {}", words[i]) };
            if self.measure(&cand, size, w, 0.0) <= max || cur.is_empty() {
                cur = cand; i += 1;
            } else {
                out.push(std::mem::take(&mut cur));
                if out.len() == lines - 1 { break; }
            }
        }
        if out.len() == lines - 1 && i < words.len() {
            let rest = std::iter::once(cur.as_str()).chain(words[i..].iter().copied()).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ");
            out.push(self.ellipsize(&rest, size, w, max));
        } else if !cur.is_empty() { out.push(if out.len() + 1 >= lines { self.ellipsize(&cur, size, w, max) } else { cur }); }
        out.truncate(lines.max(1));
        out
    }
}

impl Gfx {
    /// Draw one line of text with its top-left at (x, y). Returns the advance width.
    pub fn text(&mut self, x: f32, y: f32, size: f32, w: W, c: Color, s: &str) -> f32 { self.text_t(x, y, size, w, c, s, 0.0) }

    #[allow(clippy::too_many_arguments)]
    pub fn text_t(&mut self, x: f32, y: f32, size: f32, w: W, c: Color, s: &str, track: f32) -> f32 {
        if c.3 <= 0.0 || s.is_empty() { return 0.0; }
        let px = size.round() as u16;
        let base = (y + self.text.ascent(size)).round();
        let mut pen = x.round();
        let mut prev: Option<char> = None;
        let tex = self.text.tex;
        let inv = 1.0 / ATLAS as f32;
        for ch in s.chars() {
            if let Some(p) = prev { pen += self.text.fonts[w.ix()].horizontal_kern(p, ch, size).unwrap_or(0.0); }
            let g = self.text.glyph(w, ch, px);
            if g.w > 0 && g.h > 0 {
                let gx = (pen + g.xmin).round();
                let gy = base - g.h as f32 - g.ymin;
                let uv = [g.x as f32 * inv, g.y as f32 * inv, (g.x + g.w) as f32 * inv, (g.y + g.h) as f32 * inv];
                self.glyph(tex, Rect::new(gx, gy, g.w as f32, g.h as f32), uv, c);
            }
            pen += g.adv + track;
            prev = Some(ch);
        }
        pen - x - track
    }

    /// Right-aligned text ending at x.
    pub fn text_r(&mut self, x: f32, y: f32, size: f32, w: W, c: Color, s: &str) -> f32 {
        let tw = self.text.measure(s, size, w, 0.0);
        self.text(x - tw, y, size, w, c, s);
        tw
    }

    /// Centred text.
    pub fn text_c(&mut self, cx: f32, y: f32, size: f32, w: W, c: Color, s: &str) -> f32 {
        let tw = self.text.measure(s, size, w, 0.0);
        self.text(cx - tw / 2.0, y, size, w, c, s);
        tw
    }
}
