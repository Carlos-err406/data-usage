//! Small stacked bar charts, rendered to PNG for the SwiftBar dropdown.
//!
//! SwiftBar has no width/height parameter for base64 images, so a PNG displays
//! at whatever point size it claims. We render at 2x and stamp a pHYs chunk
//! declaring 144 DPI, which makes AppKit treat it as a half-size image —
//! sharp on Retina instead of upscaled and soft.

use crate::classify::Class;
use crate::store::Totals;

const SCALE: u32 = 2;
const DPI: u32 = 72 * SCALE;
/// pHYs is expressed in pixels per metre.
const PPU: u32 = (DPI as f32 / 0.0254) as u32;

pub struct Palette {
    pub mobile: [u8; 3],
    pub wifi: [u8; 3],
    pub wired: [u8; 3],
    pub baseline: [u8; 4],
}

/// One palette for both themes, deliberately.
///
/// SwiftBar's `image=light,dark` selection is inverted — it hands back the
/// *light* image when the theme is dark (MenuLineParameters.getImage) — so a
/// two-variant image shows the wrong one. These are Apple system colours,
/// saturated enough to read on either background, with a mid-grey baseline
/// that disappears politely into both.
pub const PALETTE: Palette = Palette {
    mobile: [255, 149, 0],
    wifi: [10, 132, 255],
    wired: [52, 199, 89],
    baseline: [128, 128, 128, 90],
};

impl Palette {
    fn color(&self, c: Class) -> [u8; 3] {
        match c {
            Class::Mobile => self.mobile,
            Class::Wifi => self.wifi,
            Class::Wired => self.wired,
        }
    }
}

struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Canvas {
    fn new(w: u32, h: u32) -> Self {
        Canvas {
            w,
            h,
            px: vec![0; (w * h * 4) as usize],
        }
    }

    /// Source-over blend, so translucent baselines sit on a clear background.
    fn blend(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        if x >= self.w || y >= self.h {
            return;
        }
        let i = ((y * self.w + x) * 4) as usize;
        let a = rgba[3] as u32;
        if a == 255 {
            self.px[i..i + 4].copy_from_slice(&rgba);
            return;
        }
        for (dst, &src) in self.px[i..i + 3].iter_mut().zip(&rgba[..3]) {
            *dst = ((src as u32 * a + *dst as u32 * (255 - a)) / 255) as u8;
        }
        self.px[i + 3] = (a + self.px[i + 3] as u32 * (255 - a) / 255).min(255) as u8;
    }

    fn rect(&mut self, x: u32, y: u32, w: u32, h: u32, rgba: [u8; 4]) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.blend(xx, yy, rgba);
            }
        }
    }

    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, self.w, self.h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.set_pixel_dims(Some(png::PixelDimensions {
                xppu: PPU,
                yppu: PPU,
                unit: png::Unit::Meter,
            }));
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&self.px).expect("png data");
        }
        out
    }
}

/// Stacked bars, one per slot, classes stacked bottom-up.
///
/// `width_pt`/`height_pt` are in points; the bitmap is SCALE times larger.
pub fn bars(series: &[(i64, Totals)], width_pt: u32, height_pt: u32, pal: &Palette) -> Vec<u8> {
    let w = width_pt * SCALE;
    let h = height_pt * SCALE;
    let mut cv = Canvas::new(w, h);

    let baseline_h = SCALE;
    let plot_h = h.saturating_sub(baseline_h + SCALE);

    let peak = series
        .iter()
        .map(|(_, t)| t.values().map(|(rx, tx)| rx + tx).sum::<u64>())
        .max()
        .unwrap_or(0);

    if !series.is_empty() && peak > 0 {
        let n = series.len() as u32;
        // Leave a hairline gap between bars, but never let a bar vanish.
        let slot = w / n;
        let gap = if slot > 3 * SCALE { SCALE } else { 0 };
        let bar_w = (slot - gap).max(1);

        for (i, (_, totals)) in series.iter().enumerate() {
            let x = (i as u32 * w) / n;
            let mut y_bottom = h - baseline_h;
            for class in Class::ALL {
                let Some((rx, tx)) = totals.get(&class) else {
                    continue;
                };
                let v = rx + tx;
                if v == 0 {
                    continue;
                }
                // Round up so a non-zero slot always paints at least one pixel.
                let seg = ((v as u128 * plot_h as u128).div_ceil(peak as u128)) as u32;
                let seg = seg.max(1).min(y_bottom);
                let c = pal.color(class);
                cv.rect(x, y_bottom - seg, bar_w, seg, [c[0], c[1], c[2], 255]);
                y_bottom -= seg;
            }
        }
    }

    cv.rect(0, h - baseline_h, w, baseline_h, pal.baseline);
    cv.encode()
}
