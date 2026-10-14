//! GLES2 2D renderer. One shader draws everything: anti-aliased rounded rectangles (signed distance),
//! soft shadows, rings, vertex-colour gradients, textured images and glyphs. Output is premultiplied
//! alpha because the compositor blends this surface over the hardware video plane: wherever we leave
//! alpha 0 the video shows through.
pub mod images;
pub mod text;

use glow::HasContext;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub f32, pub f32, pub f32, pub f32);

impl Color {
    pub const fn hex(rgb: u32, a: f32) -> Color {
        Color(((rgb >> 16) & 255) as f32 / 255.0, ((rgb >> 8) & 255) as f32 / 255.0, (rgb & 255) as f32 / 255.0, a)
    }
    pub fn a(self, a: f32) -> Color { Color(self.0, self.1, self.2, self.3 * a) }
    pub fn lerp(self, o: Color, t: f32) -> Color {
        Color(self.0 + (o.0 - self.0) * t, self.1 + (o.1 - self.1) * t, self.2 + (o.2 - self.2) * t, self.3 + (o.3 - self.3) * t)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect { pub x: f32, pub y: f32, pub w: f32, pub h: f32 }

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Rect { Rect { x, y, w, h } }
    pub fn inset(self, d: f32) -> Rect { Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d) }
    pub fn scale_center(self, s: f32) -> Rect {
        let (nw, nh) = (self.w * s, self.h * s);
        Rect::new(self.x - (nw - self.w) / 2.0, self.y - (nh - self.h) / 2.0, nw, nh)
    }
    pub fn contains(&self, x: f32, y: f32) -> bool { x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h }
    pub fn right(&self) -> f32 { self.x + self.w }
    pub fn bottom(&self) -> f32 { self.y + self.h }
    pub fn offset(self, dx: f32, dy: f32) -> Rect { Rect::new(self.x + dx, self.y + dy, self.w, self.h) }
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex { pos: [f32; 2], uv: [f32; 2], col: [f32; 4], local: [f32; 2], rect: [f32; 4], mode: f32 }

const MODE_SOLID: f32 = 0.0;
const MODE_IMAGE: f32 = 1.0;
const MODE_GLYPH: f32 = 2.0;
const MODE_RING: f32 = 3.0;
/// square-cornered quads (full-screen washes, backdrops): no distance field, just colour x texture
const MODE_PLAIN: f32 = 4.0;

pub struct Texture { pub tex: glow::Texture, pub w: u32, pub h: u32 }

const VS: &str = r#"
attribute vec2 a_pos; attribute vec2 a_uv; attribute vec4 a_col; attribute vec2 a_local; attribute vec4 a_rect; attribute float a_mode;
uniform vec2 u_screen;
varying vec2 v_uv; varying vec4 v_col; varying vec2 v_local; varying vec4 v_rect; varying float v_mode;
void main() {
  v_uv = a_uv; v_col = a_col; v_local = a_local; v_rect = a_rect; v_mode = a_mode;
  gl_Position = vec4(a_pos.x / u_screen.x * 2.0 - 1.0, 1.0 - a_pos.y / u_screen.y * 2.0, 0.0, 1.0);
}"#;

const FS: &str = r#"
precision highp float;
varying vec2 v_uv; varying vec4 v_col; varying vec2 v_local; varying vec4 v_rect; varying float v_mode;
uniform sampler2D u_tex;
float sdRound(vec2 p, vec2 b, float r) { vec2 q = abs(p) - b + vec2(r); return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r; }
void main() {
  if (v_mode > 3.5) { vec4 p = v_col * texture2D(u_tex, v_uv); gl_FragColor = vec4(p.rgb * p.a, p.a); return; }
  vec4 c = v_col;
  float cov = 1.0;
  if (v_mode > 1.5 && v_mode < 2.5) {
    c.a *= texture2D(u_tex, v_uv).a;
  } else {
    float d = sdRound(v_local, v_rect.xy, v_rect.z);
    float s = max(v_rect.w, 0.6);
    cov = 1.0 - smoothstep(-s, s, d);
    if (v_mode > 2.5) cov *= smoothstep(-s, s, d + v_uv.x);
    if (v_mode > 0.5 && v_mode < 1.5) c *= texture2D(u_tex, v_uv);
  }
  gl_FragColor = vec4(c.rgb * c.a, c.a) * cov;
}"#;

pub struct Gfx {
    pub gl: glow::Context,
    prog: glow::Program,
    vbo: glow::Buffer,
    ibo: glow::Buffer,
    verts: Vec<Vertex>,
    idx: Vec<u16>,
    cur_tex: Option<glow::Texture>,
    white: glow::Texture,
    clips: Vec<Rect>,
    pub w: f32,
    pub h: f32,
    pub alpha: f32,
    pub text: text::Text,
    pub draws: u32,
    pub verts_drawn: u32,
    pub flush_us: u64,
    /// the screens' shared background, baked once (see ui::background)
    pub bg: Option<Texture>,
    /// baked backdrop overlays by area size (see ui::Backdrop)
    pub overlays: Vec<((u32, u32), Texture)>,
}

impl Gfx {
    pub unsafe fn new(gl: glow::Context, w: f32, h: f32) -> Gfx {
        let prog = gl.create_program().unwrap();
        for (ty, src) in [(glow::VERTEX_SHADER, VS), (glow::FRAGMENT_SHADER, FS)] {
            let sh = gl.create_shader(ty).unwrap();
            gl.shader_source(sh, src);
            gl.compile_shader(sh);
            if !gl.get_shader_compile_status(sh) { crate::log!("shader: {}", gl.get_shader_info_log(sh)); }
            gl.attach_shader(prog, sh);
        }
        for (i, n) in ["a_pos", "a_uv", "a_col", "a_local", "a_rect", "a_mode"].iter().enumerate() { gl.bind_attrib_location(prog, i as u32, n); }
        gl.link_program(prog);
        if !gl.get_program_link_status(prog) { crate::log!("link: {}", gl.get_program_info_log(prog)); }
        let vbo = gl.create_buffer().unwrap();
        let ibo = gl.create_buffer().unwrap();
        let white = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(white));
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA as i32, 1, 1, 0, glow::RGBA, glow::UNSIGNED_BYTE, Some(&[255u8, 255, 255, 255][..]));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
        let text = text::Text::new(&gl);
        Gfx { gl, prog, vbo, ibo, verts: Vec::with_capacity(16384), idx: Vec::with_capacity(24576), cur_tex: None, white, clips: vec![], w, h, alpha: 1.0, text, draws: 0, verts_drawn: 0, flush_us: 0, bg: None, overlays: vec![] }
    }

    pub unsafe fn begin(&mut self, clear: Color) {
        let gl = &self.gl;
        gl.viewport(0, 0, self.w as i32, self.h as i32);
        gl.disable(glow::SCISSOR_TEST);
        gl.clear_color(clear.0 * clear.3, clear.1 * clear.3, clear.2 * clear.3, clear.3);
        gl.clear(glow::COLOR_BUFFER_BIT);
        gl.enable(glow::BLEND);
        gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
        gl.use_program(Some(self.prog));
        let loc = gl.get_uniform_location(self.prog, "u_screen");
        gl.uniform_2_f32(loc.as_ref(), self.w, self.h);
        let t = gl.get_uniform_location(self.prog, "u_tex");
        gl.uniform_1_i32(t.as_ref(), 0);
        self.draws = 0;
        self.verts_drawn = 0;
        self.flush_us = 0;
        self.text.prof_us = 0;
        self.alpha = 1.0;
        self.clips.clear();
    }

    pub unsafe fn end(&mut self) { self.flush(); self.text.upload(&self.gl); }

    unsafe fn flush(&mut self) {
        if self.idx.is_empty() { return; }
        let t0 = std::time::Instant::now();
        // glyphs added this frame must be on the GPU before they are sampled
        self.text.upload(&self.gl);
        let gl = &self.gl;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
        let vb = std::slice::from_raw_parts(self.verts.as_ptr() as *const u8, self.verts.len() * std::mem::size_of::<Vertex>());
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, vb, glow::STREAM_DRAW);
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(self.ibo));
        let ib = std::slice::from_raw_parts(self.idx.as_ptr() as *const u8, self.idx.len() * 2);
        gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, ib, glow::STREAM_DRAW);
        let stride = std::mem::size_of::<Vertex>() as i32;
        let attrs: [(u32, i32, i32); 6] = [(0, 2, 0), (1, 2, 8), (2, 4, 16), (3, 2, 32), (4, 4, 40), (5, 1, 56)];
        for (i, n, off) in attrs { gl.enable_vertex_attrib_array(i); gl.vertex_attrib_pointer_f32(i, n, glow::FLOAT, false, stride, off); }
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(self.cur_tex.unwrap_or(self.white)));
        gl.draw_elements(glow::TRIANGLES, self.idx.len() as i32, glow::UNSIGNED_SHORT, 0);
        self.draws += 1;
        self.verts_drawn += self.verts.len() as u32;
        self.verts.clear();
        self.idx.clear();
        self.flush_us += t0.elapsed().as_micros() as u64;
    }

    fn use_tex(&mut self, t: Option<glow::Texture>) {
        let t = t.or(Some(self.white));
        let cur = self.cur_tex.or(Some(self.white));
        if cur != t { unsafe { self.flush(); } self.cur_tex = t; }
        if self.verts.len() > 60000 { unsafe { self.flush(); } }
    }

    pub fn push_clip(&mut self, r: Rect) {
        let r = match self.clips.last() { Some(c) => intersect(*c, r), None => r };
        unsafe { self.flush(); }
        self.clips.push(r);
        self.apply_clip();
    }
    pub fn pop_clip(&mut self) { unsafe { self.flush(); } self.clips.pop(); self.apply_clip(); }
    fn apply_clip(&self) {
        unsafe {
            match self.clips.last() {
                Some(r) => {
                    self.gl.enable(glow::SCISSOR_TEST);
                    self.gl.scissor(r.x.max(0.0) as i32, (self.h - r.bottom()).max(0.0) as i32, r.w.max(0.0) as i32, r.h.max(0.0) as i32);
                }
                None => self.gl.disable(glow::SCISSOR_TEST),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quad(&mut self, r: Rect, uv: [f32; 4], cols: [Color; 4], radius: f32, soft: f32, mode: f32, extra: f32) {
        let pad = if mode == MODE_GLYPH || mode == MODE_PLAIN { 0.0 } else { soft.max(1.0) };
        let (hw, hh) = (r.w / 2.0, r.h / 2.0);
        let q = Rect::new(r.x - pad, r.y - pad, r.w + 2.0 * pad, r.h + 2.0 * pad);
        let base = self.verts.len() as u16;
        let corners = [(q.x, q.y), (q.right(), q.y), (q.right(), q.bottom()), (q.x, q.bottom())];
        let uvs = [(uv[0], uv[1]), (uv[2], uv[1]), (uv[2], uv[3]), (uv[0], uv[3])];
        let a = self.alpha;
        for i in 0..4 {
            let (x, y) = corners[i];
            let c = cols[i];
            let (u, v) = if mode == MODE_RING { (extra, 0.0) } else if mode == MODE_SOLID { (0.0, 0.0) } else {
                // stretch the uv over the padded quad so the image stays put
                let fx = if pad > 0.0 { (x - r.x) / r.w } else { if i == 0 || i == 3 { 0.0 } else { 1.0 } };
                let fy = if pad > 0.0 { (y - r.y) / r.h } else { if i < 2 { 0.0 } else { 1.0 } };
                if pad > 0.0 { (uv[0] + (uv[2] - uv[0]) * fx, uv[1] + (uv[3] - uv[1]) * fy) } else { uvs[i] }
            };
            self.verts.push(Vertex {
                pos: [x, y], uv: [u, v], col: [c.0, c.1, c.2, c.3 * a],
                local: [x - (r.x + hw), y - (r.y + hh)], rect: [hw, hh, radius.min(hw).min(hh), soft], mode,
            });
        }
        self.idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }

    pub fn rect(&mut self, r: Rect, c: Color, radius: f32) {
        if c.3 <= 0.0 { return; }
        self.use_tex(None);
        self.quad(r, [0.0; 4], [c; 4], radius, 0.6, if radius > 0.0 { MODE_SOLID } else { MODE_PLAIN }, 0.0);
    }
    /// top colour -> bottom colour
    pub fn vgrad(&mut self, r: Rect, top: Color, bottom: Color, radius: f32) {
        self.use_tex(None);
        self.quad(r, [0.0; 4], [top, top, bottom, bottom], radius, 0.6, if radius > 0.0 { MODE_SOLID } else { MODE_PLAIN }, 0.0);
    }
    /// left colour -> right colour
    pub fn hgrad(&mut self, r: Rect, left: Color, right: Color, radius: f32) {
        self.use_tex(None);
        self.quad(r, [0.0; 4], [left, right, right, left], radius, 0.6, if radius > 0.0 { MODE_SOLID } else { MODE_PLAIN }, 0.0);
    }
    /// A texture stretched over `r` as is (no crop, no rounding): the baked background.
    pub fn blit(&mut self, t: &Texture, r: Rect) {
        self.use_tex(Some(t.tex));
        self.quad(r, [0.0, 0.0, 1.0, 1.0], [Color(1.0, 1.0, 1.0, 1.0); 4], 0.0, 0.0, MODE_PLAIN, 0.0);
    }
    pub fn shadow(&mut self, r: Rect, c: Color, radius: f32, blur: f32) {
        self.use_tex(None);
        self.quad(r, [0.0; 4], [c; 4], radius, blur, MODE_SOLID, 0.0);
    }
    pub fn ring(&mut self, r: Rect, c: Color, radius: f32, thickness: f32) {
        self.use_tex(None);
        self.quad(r, [0.0; 4], [c; 4], radius, 0.7, MODE_RING, thickness);
    }
    /// Draw a texture into `r`, cropped to cover it (like CSS object-fit: cover), with rounded corners.
    pub fn image(&mut self, t: &Texture, r: Rect, radius: f32, opacity: f32, focus_x: f32, focus_y: f32) {
        let (tw, th) = (t.w as f32, t.h as f32);
        let (sx, sy) = (r.w / tw, r.h / th);
        let s = sx.max(sy);
        let (vw, vh) = (r.w / s / tw, r.h / s / th);
        let u0 = (1.0 - vw) * focus_x;
        let v0 = (1.0 - vh) * focus_y;
        self.use_tex(Some(t.tex));
        let mode = if radius > 0.0 { MODE_IMAGE } else { MODE_PLAIN };
        self.quad(r, [u0, v0, u0 + vw, v0 + vh], [Color(1.0, 1.0, 1.0, opacity); 4], radius, 0.6, mode, 0.0);
    }
    /// Part of a texture (uv = u0, v0, u1, v1), e.g. one thumbnail of a sprite sheet.
    pub fn image_uv(&mut self, t: &Texture, r: Rect, uv: [f32; 4], radius: f32, opacity: f32) {
        self.use_tex(Some(t.tex));
        let mode = if radius > 0.0 { MODE_IMAGE } else { MODE_PLAIN };
        self.quad(r, uv, [Color(1.0, 1.0, 1.0, opacity); 4], radius, 0.6, mode, 0.0);
    }
    pub(crate) fn glyph(&mut self, tex: glow::Texture, r: Rect, uv: [f32; 4], c: Color) {
        self.use_tex(Some(tex));
        self.quad(r, uv, [c; 4], 0.0, 0.0, MODE_GLYPH, 0.0);
    }
    /// A filled triangle (icons). Uses a degenerate quad in solid mode with zero radius.
    pub fn tri(&mut self, p: [(f32, f32); 3], c: Color) {
        self.use_tex(None);
        let base = self.verts.len() as u16;
        let a = self.alpha;
        for &(x, y) in p.iter().chain(std::iter::once(&p[2])) {
            // a huge "rect" around each vertex: the SDF is always deep inside, so coverage is 1
            self.verts.push(Vertex { pos: [x, y], uv: [0.0, 0.0], col: [c.0, c.1, c.2, c.3 * a], local: [0.0, 0.0], rect: [1e5, 1e5, 0.0, 0.6], mode: MODE_SOLID });
        }
        self.idx.extend_from_slice(&[base, base + 1, base + 2]);
    }
    /// A thick line segment with rounded ends (icons: check marks, arrows).
    pub fn line(&mut self, a: (f32, f32), b: (f32, f32), w: f32, c: Color) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = (dx * dx + dy * dy).sqrt().max(0.001);
        let (nx, ny) = (-dy / len * w / 2.0, dx / len * w / 2.0);
        self.tri([(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny)], c);
        self.tri([(a.0 + nx, a.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)], c);
        let r = w / 2.0;
        self.rect(Rect::new(a.0 - r, a.1 - r, w, w), c, r);
        self.rect(Rect::new(b.0 - r, b.1 - r, w, w), c, r);
    }

    pub unsafe fn upload_rgba(&self, w: u32, h: u32, px: &[u8]) -> Texture {
        let gl = &self.gl;
        let tex = gl.create_texture().unwrap();
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA as i32, w as i32, h as i32, 0, glow::RGBA, glow::UNSIGNED_BYTE, Some(px));
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        // whatever texture the batch had bound must be re-bound at flush
        Texture { tex, w, h }
    }

    pub unsafe fn free(&self, t: Texture) { self.gl.delete_texture(t.tex); }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
}
