//! Draws the overlay (text with real shaping, pictures, boxes) into a transparent RGBA canvas the
//! size of the video. The compositor in `mixer.rs` blends that canvas onto the live frames.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use cosmic_text::{fontdb, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache, Weight};
use tiny_skia::{FilterQuality, Paint, Pixmap, PixmapPaint, Rect, Transform};

use crate::overlay::{self, ItemKind, OverlayItem};

fn font_files(extra: Option<&Path>) -> Vec<PathBuf> {
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &[
            "/System/Library/Fonts/Supplemental/Arial.ttf",
            "/System/Library/Fonts/Supplemental/Arial Bold.ttf",
            "/System/Library/Fonts/Supplemental/Ayuthaya.ttf", // Thai
        ]
    } else if cfg!(windows) {
        &["C:/Windows/Fonts/arial.ttf", "C:/Windows/Fonts/arialbd.ttf", "C:/Windows/Fonts/tahoma.ttf", "C:/Windows/Fonts/leelawui.ttf"]
    } else {
        &["/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"]
    };
    extra.into_iter().map(PathBuf::from).chain(candidates.iter().map(PathBuf::from)).filter(|p| p.exists()).collect()
}

pub struct Renderer {
    fs: FontSystem,
    swash: SwashCache,
    family: Option<String>,
    images: HashMap<PathBuf, Option<Pixmap>>,
    pub w: u32,
    pub h: u32,
}

impl Renderer {
    /// Loads only a few font files instead of every system font (which would cost tens of MB).
    pub fn new(w: u32, h: u32, font_file: Option<&Path>) -> Self {
        let sources = font_files(font_file).into_iter().map(fontdb::Source::File);
        let fs = FontSystem::new_with_fonts(sources);
        let family = fs.db().faces().next().and_then(|f| f.families.first().map(|(n, _)| n.clone()));
        Self { fs, swash: SwashCache::new(), family, images: HashMap::new(), w, h }
    }

    /// Straight-alpha RGBA, `w * h * 4` bytes.
    pub fn render(&mut self, items: &[OverlayItem], channel: u32, data: &BTreeMap<String, String>) -> Vec<u8> {
        let mut pm = Pixmap::new(self.w, self.h).expect("canvas");
        for it in items.iter().filter(|i| i.enabled).take(overlay::MAX_ITEMS) {
            match it.kind {
                ItemKind::Text => self.draw_text(&mut pm, it, channel, data),
                ItemKind::Image => self.draw_image(&mut pm, it),
            }
        }
        let mut out = pm.take();
        for px in out.chunks_exact_mut(4) {
            let a = px[3] as u32;
            if a > 0 && a < 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8; // un-premultiply
                }
            }
        }
        out
    }

    fn draw_text(&mut self, pm: &mut Pixmap, it: &OverlayItem, channel: u32, data: &BTreeMap<String, String>) {
        let Some(family) = self.family.clone() else { return };
        let text = overlay::render(&it.text, channel, data);
        let px = (it.size.clamp(0.01, 0.5) * self.h as f32).max(6.0);
        let lh = px * 1.2;
        let Self { fs, swash, w, h, .. } = self;
        let mut buf = Buffer::new(fs, Metrics::new(px, lh));
        buf.set_size(None, None);
        let attrs = Attrs::new().family(Family::Name(&family)).weight(if it.bold { Weight::BOLD } else { Weight::NORMAL });
        buf.set_text(&text, &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(fs, false);
        let (mut width, mut lines) = (0f32, 0f32);
        for run in buf.layout_runs() {
            width = width.max(run.line_w);
            lines += 1.0;
        }
        let (left, top) = (it.x.clamp(0.0, 1.0) * *w as f32, it.y.clamp(0.0, 1.0) * *h as f32);
        let boxed = it.bg_opacity > 0.0;
        let (padx, pady) = if boxed { (0.3 * px, 0.15 * px) } else { (0.0, 0.0) };
        if boxed {
            if let (Some(c), Some(r)) = (rgb(&it.bg_color), Rect::from_xywh(left, top, width + 2.0 * padx, lines * lh + 2.0 * pady)) {
                let mut p = Paint::default();
                p.set_color_rgba8(c.0, c.1, c.2, (it.bg_opacity.clamp(0.0, 1.0) * 255.0) as u8);
                pm.fill_rect(r, &p, Transform::identity(), None);
            }
        }
        let c = rgb(&it.color).unwrap_or((255, 255, 255));
        let mut p = Paint::default();
        buf.draw(fs, swash, Color::rgb(c.0, c.1, c.2), |gx, gy, gw, gh, col| {
            if col.a() == 0 {
                return;
            }
            if let Some(r) = Rect::from_xywh(left + padx + gx as f32, top + pady + gy as f32, gw as f32, gh as f32) {
                p.set_color_rgba8(col.r(), col.g(), col.b(), col.a());
                pm.fill_rect(r, &p, Transform::identity(), None);
            }
        });
    }

    fn draw_image(&mut self, pm: &mut Pixmap, it: &OverlayItem) {
        let Some(path) = it.image.clone() else { return };
        let img = self.images.entry(path.clone()).or_insert_with(|| load_pixmap(&path));
        let Some(img) = img else { return };
        let scale = (it.width.clamp(0.01, 1.0) * self.w as f32) / img.width() as f32;
        let t = Transform::from_row(scale, 0.0, 0.0, scale, it.x.clamp(0.0, 1.0) * self.w as f32, it.y.clamp(0.0, 1.0) * self.h as f32);
        let paint = PixmapPaint { opacity: it.opacity.clamp(0.0, 1.0), quality: FilterQuality::Bilinear, ..Default::default() };
        pm.draw_pixmap(0, 0, img.as_ref(), &paint, t, None);
    }
}

fn rgb(hex: &str) -> Option<(u8, u8, u8)> {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

fn load_pixmap(path: &Path) -> Option<Pixmap> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        for c in &mut px[..3] {
            *c = ((*c as u32 * a + 127) / 255) as u8; // premultiply
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h)?)
}
