//! Download cards and hero for Home's "Downloading" row (data: feed.rs). The picture is greyed out - it isn't in the
//! library yet - and fills with colour from the left as the download progresses; an arrow keeps dropping into a tray.
use super::*;
use crate::feed::Download;
use crate::gfx::images::gray;

/// The animated download glyph in an s*s box: an arrow falling into a tray (still when `moving` is false).
pub fn glyph(g: &mut Gfx, x: f32, y: f32, s: f32, t: f32, c: Color, moving: bool) {
    let u = s / 24.0;
    let p = |a: f32, b: f32| (x + a * u, y + b * u);
    // tray
    g.line(p(4.0, 15.0), p(4.0, 20.0), 2.4 * u, c);
    g.line(p(4.0, 20.0), p(20.0, 20.0), 2.4 * u, c);
    g.line(p(20.0, 20.0), p(20.0, 15.0), 2.4 * u, c);
    // arrow: falls from above the box into the tray, fading in and out
    let (o, a) = if moving {
        let k = (t * 0.85).fract();
        let e = k * k * (3.0 - 2.0 * k);
        (-9.0 + 11.0 * e, (k * std::f32::consts::PI).sin().powf(0.6))
    } else { (0.0, 1.0) };
    g.push_clip(Rect::new(x - 2.0 * u, y - 2.0 * u, 28.0 * u, 19.5 * u));
    let ca = c.a(a);
    g.line(p(12.0, 2.0 + o), p(12.0, 11.0 + o), 2.6 * u, ca);
    g.tri([p(6.5, 9.0 + o), p(17.5, 9.0 + o), p(12.0, 15.5 + o)], ca);
    g.pop_clip();
}

/// A landscape card: greyed picture, coloured up to the progress, bar + percentage, title and status under it.
pub fn card(g: &mut Gfx, cx: &mut Cx, d: &Download, age: f32, r: Rect, f: f32) {
    let rr = r.scale_center(1.0 + 0.06 * f);
    let moving = d.state == "downloading";
    let (p, _) = d.live(age);
    focus_ring(g, rr, 16.0, f);
    picture(g, cx, &gray(&d.thumb), rr, 16.0, &initial(&d.title));
    // the part that is already here, in colour
    if p > 0.0 {
        if let Some((tex, a)) = cx.images.get(&d.thumb).map(|(t, a)| (t as *const crate::gfx::Texture, a)) {
            g.push_clip(Rect::new(rr.x, rr.y, rr.w * p, rr.h));
            g.image(unsafe { &*tex }, rr, 16.0, a, 0.5, 0.25);
            g.pop_clip();
        }
        if moving && p < 1.0 {
            // a soft, breathing light along the edge of the colour
            let ex = rr.x + rr.w * p;
            let glow = 0.16 + 0.10 * (cx.t * 2.4).sin();
            g.push_clip(rr);
            g.hgrad(Rect::new(ex - 30.0, rr.y, 30.0, rr.h), Color(1.0, 1.0, 1.0, 0.0), Color(1.0, 1.0, 1.0, glow), 0.0);
            g.rect(Rect::new(ex - 1.0, rr.y, 2.0, rr.h), Color(1.0, 1.0, 1.0, glow + 0.2), 0.0);
            g.pop_clip();
        }
    }
    g.vgrad(Rect::new(rr.x, rr.y + rr.h * 0.42, rr.w, rr.h * 0.58), Color(0.0, 0.0, 0.0, 0.0), Color(0.0, 0.0, 0.0, 0.72), 16.0);
    // badge with the falling arrow
    let b = Rect::new(rr.x + 12.0, rr.y + 12.0, 44.0, 44.0);
    g.rect(b, Color(0.03, 0.03, 0.05, 0.72), 22.0);
    glyph(g, b.x + 10.0, b.y + 9.0, 24.0, cx.t, if moving { TEXT } else { MUTED }, moving);
    // percentage + bar
    let bar = Rect::new(rr.x + 14.0, rr.bottom() - 18.0, rr.w - 28.0, 6.0);
    if d.state != "queued" {
        g.text(rr.x + 16.0, bar.y - 40.0, 28.0, W::Bold, TEXT, &format!("{} %", (p * 100.0).floor() as i32));
    }
    g.rect(bar, Color(1.0, 1.0, 1.0, 0.18), 3.0);
    let fill = (bar.w * p).max(6.0);
    g.rect(Rect::new(bar.x, bar.y, fill, bar.h), if moving || d.state == "importing" { ACCENT } else { MUTED }, 3.0);
    if moving {
        let sx = bar.x + ((cx.t * 0.55).fract() * (fill + 60.0)) - 60.0;
        g.push_clip(Rect::new(bar.x, bar.y, fill, bar.h));
        g.hgrad(Rect::new(sx, bar.y, 30.0, bar.h), Color(1.0, 1.0, 1.0, 0.0), Color(1.0, 1.0, 1.0, 0.65), 0.0);
        g.hgrad(Rect::new(sx + 30.0, bar.y, 30.0, bar.h), Color(1.0, 1.0, 1.0, 0.65), Color(1.0, 1.0, 1.0, 0.0), 0.0);
        g.pop_clip();
    }
    // caption
    let tw = r.w - 6.0;
    let title = [d.title.as_str(), d.label.as_str()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join("  ·  ");
    let t = g.text.ellipsize(&title, 22.0, W::Semi, tw);
    g.text(r.x + 4.0, r.bottom() + 16.0 + 8.0 * f, 22.0, W::Semi, TEXT.a(0.85 + 0.15 * f), &t);
    let s = g.text.ellipsize(&d.status_line(age), 18.0, W::Regular, tw);
    g.text(r.x + 4.0, r.bottom() + 46.0 + 8.0 * f, 18.0, W::Regular, MUTED, &s);
}

/// The hero text block for a focused download (the greyed backdrop is drawn by Home's Backdrop).
pub fn hero(g: &mut Gfx, cx: &mut Cx, d: &Download, age: f32, x: f32, bottom: f32, alpha: f32) {
    let prev = g.alpha;
    g.alpha *= alpha;
    let ov = g.text.wrap(&d.overview.replace('\n', " "), 22.0, W::Regular, 680.0, 2);
    let mut y = bottom - ov.len() as f32 * 33.0;
    for (i, l) in ov.iter().enumerate() { g.text(x, y + i as f32 * 33.0, 22.0, W::Regular, TEXT.a(0.72), l); }
    y -= 46.0;
    let mut meta: Vec<String> = vec![];
    if let Some(yr) = d.year { meta.push(yr.to_string()); }
    meta.push(if d.kind == "movie" { "Film".into() } else { "Series".into() });
    if d.size > 0 { meta.push(crate::feed::size(d.size)); }
    meta.push("Not in your library yet".into());
    g.text(x, y, 20.0, W::Regular, MUTED, &meta.join("  ·  "));
    // progress
    let (p, _) = d.live(age);
    let moving = d.state == "downloading";
    y -= 62.0;
    let bar = Rect::new(x, y + 44.0, 680.0, 8.0);
    g.rect(bar, Color(1.0, 1.0, 1.0, 0.16), 4.0);
    let fill = (bar.w * p).max(8.0);
    g.rect(Rect::new(bar.x, bar.y, fill, bar.h), if moving || d.state == "importing" { ACCENT } else { MUTED }, 4.0);
    if moving {
        let sx = bar.x + ((cx.t * 0.4).fract() * (fill + 120.0)) - 120.0;
        g.push_clip(Rect::new(bar.x, bar.y, fill, bar.h));
        g.hgrad(Rect::new(sx, bar.y, 60.0, bar.h), Color(1.0, 1.0, 1.0, 0.0), Color(1.0, 1.0, 1.0, 0.6), 0.0);
        g.hgrad(Rect::new(sx + 60.0, bar.y, 60.0, bar.h), Color(1.0, 1.0, 1.0, 0.6), Color(1.0, 1.0, 1.0, 0.0), 0.0);
        g.pop_clip();
    }
    glyph(g, x, y + 2.0, 30.0, cx.t, if moving { TEXT } else { MUTED }, moving);
    let st = g.text.ellipsize(&d.status_line(age), 26.0, W::Semi, 630.0);
    g.text(x + 44.0, y + 2.0, 26.0, W::Semi, TEXT, &st);
    // what exactly: "S02 · 9 episodes", or the episode
    let sub = [d.label.as_str(), d.name.as_str()].iter().filter(|s| !s.is_empty() && **s != "TBA").cloned().collect::<Vec<_>>().join("  ·  ");
    if !sub.is_empty() { y -= 48.0; let s = g.text.ellipsize(&sub, 28.0, W::Medium, 820.0); g.text(x, y, 28.0, W::Medium, TEXT, &s); }
    // logo, else the title
    let mut drew_logo = false;
    if !d.logo.is_empty() {
        if let Some((tex, a)) = cx.images.get(&d.logo) {
            let tex = unsafe { &*(tex as *const crate::gfx::Texture) };
            let s = (560.0 / tex.w as f32).min(170.0 / tex.h as f32);
            let (w, h) = (tex.w as f32 * s, tex.h as f32 * s);
            y -= h + 22.0;
            g.image(tex, Rect::new(x, y, w, h), 0.0, a, 0.5, 0.5);
            drew_logo = true;
        }
    }
    if !drew_logo {
        let lines = g.text.wrap(&d.title, 76.0, W::Bold, 860.0, 2);
        y -= lines.len() as f32 * 80.0 + 18.0;
        for (i, l) in lines.iter().enumerate() { g.text_t(x, y + i as f32 * 80.0, 76.0, W::Bold, TEXT, l, -1.6); }
    }
    y -= 58.0;
    let dot = if moving { ACCENT.a(0.55 + 0.45 * (cx.t * 3.0).sin().abs()) } else if matches!(d.state.as_str(), "blocked" | "failed" | "warning" | "stalled") { Color::hex(0xff9b9b, 1.0) } else { MUTED };
    pill(g, x, y, d.pill(), dot);
    g.alpha = prev;
}
