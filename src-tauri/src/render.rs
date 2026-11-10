//! Non-star-separated CPU mirror of the editor's compositor.
//!
//! All science comes from the full planes. Star labels/backgrounds/overrides are
//! intentionally irrelevant to this milestone. Values remain float32 through
//! compositing and output tone; encoding does not add an implicit sRGB gamma.
use crate::contract::RenderedImage;
use crate::geometry::{View, boolean, canvas_extent, dimensions, number, rounded_dimension};
use crate::planes::{
    Plane, check_cancelled, edge_distance_scaled, filled, load_coverage, pixel_count, project_file,
};
use anyhow::{Context, Result, bail};
use image::{ExtendedColorType, ImageEncoder};
use rayon::prelude::*;
use serde_json::Value;
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Seek, Write};
use std::path::Path;

const BLENDS: [&str; 10] = [
    "normal",
    "add",
    "screen",
    "multiply",
    "overlay",
    "softlight",
    "lighten",
    "difference",
    "subtract",
    "darken",
];

fn object(value: &Value, label: &str) -> Result<()> {
    if !value.is_null() && !value.is_object() {
        bail!("{label} must be an object");
    }
    Ok(())
}

fn active(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.),
        Value::String(v) => !v.is_empty(),
        Value::Array(v) => !v.is_empty(),
        Value::Object(v) => !v.is_empty(),
    }
}

fn fields(value: &Value, allowed: &[&str], label: &str) -> Result<()> {
    object(value, label)?;
    if let Some(map) = value.as_object() {
        for (key, value) in map {
            if !allowed.contains(&key.as_str()) && active(value) {
                bail!("unsupported active {label} field: {key}");
            }
        }
    }
    Ok(())
}

fn text<'a>(value: &'a Value, key: &str, default: &'a str) -> Result<&'a str> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("{key} must be a string")),
    }
}

fn scalar(value: &Value, key: &str, default: f32) -> Result<f32> {
    let n = number(value, key, default as f64)?;
    let n = n as f32;
    if !n.is_finite() {
        bail!("{key} exceeds float32 range");
    }
    Ok(n)
}

fn rgb(hex: &str) -> Result<[f32; 3]> {
    if hex.len() != 7 || !hex.starts_with('#') || !hex[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid RGB colour {hex:?}; expected #rrggbb");
    }
    let mut result = [0.; 3];
    for (i, channel) in result.iter_mut().enumerate() {
        *channel = u8::from_str_radix(&hex[1 + i * 2..3 + i * 2], 16)? as f32 / 255.;
    }
    Ok(result)
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}

#[derive(Clone, Copy)]
pub(crate) struct Transform {
    x: f32,
    y: f32,
    scale: f32,
    c: f32,
    s: f32,
}

impl Transform {
    pub(crate) fn parse(value: &Value) -> Result<Self> {
        fields(value, &["x", "y", "scale", "rotation"], "layer transform")?;
        let angle = number(value, "rotation", 0.)?.to_radians();
        Ok(Self {
            x: scalar(value, "x", 0.)?,
            y: scalar(value, "y", 0.)?,
            scale: scalar(value, "scale", 1.)?.max(0.001),
            c: angle.cos() as f32,
            s: angle.sin() as f32,
        })
    }

    pub(crate) fn point(&self, x: f32, y: f32, width: f32, height: f32) -> (f32, f32) {
        // Python warp returns its source unchanged for the identity transform.
        // Avoid subtract/add roundoff that can manufacture tiny coverage at a
        // missing pixel's exact centre on non-power-of-two dimensions.
        if self.x == 0. && self.y == 0. && self.scale == 1. && self.c == 1. && self.s == 0. {
            return (x, y);
        }
        let px = (x - 0.5 - self.x) * width;
        let py = (y - 0.5 - self.y) * height;
        (
            (self.c * px + self.s * py) / width / self.scale + 0.5,
            (-self.s * px + self.c * py) / height / self.scale + 0.5,
        )
    }
}

struct Clip {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    feather: f32,
    ellipse: bool,
}

impl Clip {
    fn parse(value: &Value) -> Result<Option<Self>> {
        if value.is_null() {
            return Ok(None);
        }
        fields(
            value,
            &["x", "y", "w", "h", "feather", "shape", "rotation"],
            "layer clip",
        )?;
        // LayerClip inherits Crop.rotation, but neither Python clip_coverage nor
        // the GPU clipAlpha consumes it. Preserve it as inert saved metadata;
        // global.crop.rotation remains an active anchor in geometry::View.
        let shape = text(value, "shape", "rect")?;
        if !["rect", "ellipse"].contains(&shape) {
            bail!("unsupported clip shape: {shape}");
        }
        let result = Self {
            x: scalar(value, "x", 0.)?,
            y: scalar(value, "y", 0.)?,
            w: scalar(value, "w", 1.)?,
            h: scalar(value, "h", 1.)?,
            feather: scalar(value, "feather", 0.)?,
            ellipse: shape == "ellipse",
        };
        if result.w <= 0. || result.h <= 0. || result.feather < 0. {
            bail!("layer clip dimensions must be positive and feather nonnegative");
        }
        Ok(Some(result))
    }

    fn alpha(&self, x: f32, y: f32, width: f32, height: f32) -> f32 {
        let (sx, sy) = (self.w * width, self.h * height);
        let qx = (x - self.x) / self.w.max(1e-6);
        let qy = (y - self.y) / self.h.max(1e-6);
        let short = sx.min(sy);
        let distance = if self.ellipse {
            (1. - ((qx - 0.5) * 2.).hypot((qy - 0.5) * 2.)) * short * 0.5
        } else {
            (qx * sx)
                .min((1. - qx) * sx)
                .min(qy * sy)
                .min((1. - qy) * sy)
        };
        if self.feather > 0. {
            smooth(distance / (self.feather * short))
        } else if distance >= 0. {
            1.
        } else {
            0.
        }
    }
}

struct Tone {
    black: f32,
    white: f32,
    k: f32,
    stretch: String,
    intensity: f32,
    invert: bool,
    curve: Option<[f32; 256]>,
    amount: f32,
    radius: f32,
    gain: f32,
    offset: f32,
}

impl Tone {
    fn parse(value: &Value) -> Result<Self> {
        let stretch = text(value, "stretch", "asinh")?;
        if !["linear", "asinh", "log", "gamma"].contains(&stretch) {
            bail!("unsupported stretch mode: {stretch}");
        }
        fields(&value["detail"], &["amount", "radius"], "layer detail")?;
        fields(&value["toneMatch"], &["gain", "offset"], "layer toneMatch")?;
        let mut points = Vec::new();
        if !value["curve"].is_null() {
            let curve = value["curve"]
                .as_array()
                .context("curve must be an array of {x,y} points")?;
            for point in curve {
                fields(point, &["x", "y"], "curve point")?;
                points.push((number(point, "x", 0.)?, number(point, "y", 0.)?));
            }
        }
        // Design decision: an explicit identity curve is a true no-op even above
        // white. Clamping first would break saved low-intensity looks.
        let curve = if points.iter().any(|(x, y)| x != y) {
            Some(curve_lut(&points))
        } else {
            None
        };
        let result = Self {
            black: scalar(value, "black", 0.)?,
            white: scalar(value, "white", 1.)?,
            k: scalar(value, "k", 10.)?.max(0.001),
            stretch: stretch.into(),
            intensity: scalar(value, "intensity", 1.)?,
            invert: boolean(value, "invert", false)?,
            curve,
            amount: scalar(&value["detail"], "amount", 0.)?,
            radius: scalar(&value["detail"], "radius", 8.)?.clamp(1., 64.),
            gain: scalar(&value["toneMatch"], "gain", 1.)?,
            offset: scalar(&value["toneMatch"], "offset", 0.)?,
        };
        if result.intensity < 0. || result.amount < 0. {
            bail!("layer intensity and detail amount must be nonnegative");
        }
        Ok(result)
    }

    fn base(&self, value: f32) -> f32 {
        let x = ((value - self.black) / (self.white - self.black).max(1e-5)).max(0.);
        match self.stretch.as_str() {
            "asinh" => (self.k * x).asinh() / self.k.asinh(),
            "log" => (self.k * x).ln_1p() / self.k.ln_1p(),
            "gamma" => x.powf(1. / self.k.max(0.01)),
            _ => x,
        }
    }

    fn apply(&self, value: f32, blurred: Option<f32>) -> f32 {
        let mut x = self.base(value);
        if let Some(blurred) = blurred {
            x = x.clamp(0., 1.);
            x = (x + self.amount * (x - blurred)).clamp(0., 1.);
        }
        if self.invert {
            x = 1. - x.clamp(0., 1.);
        }
        if let Some(curve) = &self.curve {
            let pos = x.clamp(0., 1.) * 255.;
            let lo = pos.floor() as usize;
            x = curve[lo] + (curve[(lo + 1).min(255)] - curve[lo]) * (pos - lo as f32);
        }
        ((x * self.intensity).clamp(0., 1.) * self.gain + self.offset).clamp(0., 1.)
    }

    fn needs_coverage(&self) -> bool {
        self.invert || self.curve.is_some()
    }
}

/// Shape-preserving cubic Hermite LUT (Fritsch-Carlson slope limiting).
fn curve_lut(points: &[(f64, f64)]) -> [f32; 256] {
    let mut sorted: Vec<_> = points
        .iter()
        .filter(|(x, y)| x.is_finite() && y.is_finite())
        .map(|(x, y)| (x.clamp(0., 1.), y.clamp(0., 1.)))
        .collect();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut p = Vec::new();
    for (i, point) in sorted.iter().enumerate() {
        if i == 0 || point.0 - sorted[i - 1].0 > 1e-6 {
            p.push(*point);
        }
    }
    if p.first().is_none_or(|v| v.0 != 0.) {
        p.insert(0, (0., 0.));
    }
    if p.last().is_none_or(|v| v.0 != 1.) {
        p.push((1., 1.));
    }
    let h: Vec<_> = p.windows(2).map(|v| v[1].0 - v[0].0).collect();
    let d: Vec<_> = p
        .windows(2)
        .zip(&h)
        .map(|(v, h)| (v[1].1 - v[0].1) / h)
        .collect();
    let mut m = vec![d[0]];
    m.extend(d.windows(2).map(|v| (v[0] + v[1]) / 2.));
    m.push(d[d.len() - 1]);
    for i in 1..p.len() - 1 {
        if d[i - 1] * d[i] <= 0. {
            m[i] = 0.;
        }
    }
    for i in 0..d.len() {
        if d[i] == 0. {
            m[i] = 0.;
            m[i + 1] = 0.;
        } else {
            let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
            let r = a.hypot(b);
            if r > 3. {
                m[i] = 3. * a / r * d[i];
                m[i + 1] = 3. * b / r * d[i];
            }
        }
    }
    std::array::from_fn(|i| {
        let x = i as f64 / 255.;
        let j = p
            .partition_point(|v| v.0 < x)
            .saturating_sub(1)
            .min(p.len() - 2);
        let t = (x - p[j].0) / h[j];
        let y = (2. * t.powi(3) - 3. * t.powi(2) + 1.) * p[j].1
            + (t.powi(3) - 2. * t.powi(2) + t) * h[j] * m[j]
            + (-2. * t.powi(3) + 3. * t.powi(2)) * p[j + 1].1
            + (t.powi(3) - t.powi(2)) * h[j] * m[j + 1];
        y.clamp(0., 1.) as f32
    })
}

fn log_position(nm: f64) -> Result<f64> {
    let nm = if nm == 0. { 550. } else { nm };
    if nm <= 0. || !nm.is_finite() {
        bail!("wavelength must be positive");
    }
    Ok(((nm / 200.).ln() / 125_f64.ln()).clamp(0., 1.))
}

fn gradient_lut(value: &Value, spectral: bool) -> Result<[[f32; 3]; 256]> {
    let default = if spectral {
        serde_json::json!([
            {"nm": 200, "color": "#436bdb"}, {"nm": 550, "color": "#5bb8ba"},
            {"nm": 2500, "color": "#dc8054"}, {"nm": 25000, "color": "#b73335"}
        ])
    } else {
        serde_json::json!([
            {"t": 0, "color": "#080c16"}, {"t": 0.45, "color": "#426f9d"},
            {"t": 0.8, "color": "#d2b17c"}, {"t": 1, "color": "#fff0d5"}
        ])
    };
    let value = if value.is_null() { &default } else { value };
    let stops = value
        .as_array()
        .context("colour gradient stops must be an array")?;
    if stops.is_empty() {
        bail!("colour gradient must contain at least one stop");
    }
    let mut stops = stops
        .iter()
        .map(|stop| {
            fields(
                stop,
                &[if spectral { "nm" } else { "t" }, "color"],
                "gradient stop",
            )?;
            let t = if spectral {
                log_position(number(stop, "nm", 550.)?)?
            } else {
                number(stop, "t", 0.)?
            };
            Ok((t, rgb(text(stop, "color", "#ffffff")?)?))
        })
        .collect::<Result<Vec<_>>>()?;
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(std::array::from_fn(|i| {
        let t = i as f64 / 255.;
        let hi = stops.partition_point(|v| v.0 < t).min(stops.len() - 1);
        let lo = hi.saturating_sub(1);
        let (a, b) = if t <= stops[0].0 {
            (stops[0], stops[0])
        } else if t >= stops[stops.len() - 1].0 {
            (stops[stops.len() - 1], stops[stops.len() - 1])
        } else {
            (stops[lo], stops[hi])
        };
        let f = ((t - a.0) / if b.0 == a.0 { 1. } else { b.0 - a.0 }).clamp(0., 1.);
        std::array::from_fn(|c| {
            // GPU uploads an RGBA8 LUT; round each entry before interpolation.
            ((a.1[c] as f64 + (b.1[c] as f64 - a.1[c] as f64) * f) * 255.).round() as f32 / 255.
        })
    }))
}

fn sample_gradient(lut: &[[f32; 3]; 256], value: f32) -> [f32; 3] {
    let pos = value.clamp(0., 1.) * 255.;
    let lo = pos.floor() as usize;
    std::array::from_fn(|c| {
        lut[lo][c] + (lut[(lo + 1).min(255)][c] - lut[lo][c]) * (pos - lo as f32)
    })
}

fn blend_channel(a: f32, b: f32, mode: &str) -> f32 {
    match mode {
        "add" => a + b,
        "screen" => 1. - (1. - a) * (1. - b),
        "multiply" => a * b,
        "overlay" => {
            if a >= 0.5 {
                1. - 2. * (1. - a) * (1. - b)
            } else {
                2. * a * b
            }
        }
        "softlight" => {
            if b >= 0.5 {
                a + (2. * b - 1.) * (a.max(0.).sqrt() - a)
            } else {
                a - (1. - 2. * b) * a * (1. - a)
            }
        }
        "lighten" => a.max(b),
        "difference" => (a - b).abs(),
        "subtract" => (a - b).max(0.),
        "darken" => a.min(b),
        _ => b,
    }
}

fn edge_hash(x: i32, y: i32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(374761393)
        .wrapping_add((y as u32).wrapping_mul(668265263));
    h = (h ^ (h >> 13)).wrapping_mul(1274126177);
    h ^= h >> 16;
    (h & 65535) as f32 / 65535.
}

fn value_noise(x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (smooth(x - x.floor()), smooth(y - y.floor()));
    let a = edge_hash(ix, iy);
    let b = edge_hash(ix.wrapping_add(1), iy);
    let c = edge_hash(ix, iy.wrapping_add(1));
    let d = edge_hash(ix.wrapping_add(1), iy.wrapping_add(1));
    let low = a + (b - a) * fx;
    let high = c + (d - c) * fx;
    low + (high - low) * fy
}

fn edge_noise(x: f32, y: f32) -> f32 {
    (value_noise(x, y) * 4. + value_noise(x * 2., y * 2.) * 2. + value_noise(x * 4., y * 4.)) / 7.
}

struct Output {
    exposure: f32,
    highlights: f32,
    black: f32,
    white: f32,
    midtones: f32,
}

impl Output {
    fn parse(value: &Value) -> Result<Self> {
        fields(
            value,
            &["exposure", "highlights", "black", "white", "midtones"],
            "output",
        )?;
        let result = Self {
            exposure: scalar(value, "exposure", 0.)?,
            highlights: scalar(value, "highlights", 0.)?,
            black: scalar(value, "black", 0.)?,
            white: scalar(value, "white", 1.)?,
            midtones: scalar(value, "midtones", 1.)?,
        };
        if !(0. ..=1.).contains(&result.highlights) || !result.exposure.exp2().is_finite() {
            bail!("output highlights must be 0..1 and exposure must have a finite gain");
        }
        if !(0. ..=1.).contains(&result.black)
            || !(0.01..=3.).contains(&result.white)
            || !(0.3..=4.).contains(&result.midtones)
        {
            bail!("output black must be 0..1, white 0.01..3 and midtones 0.3..4");
        }
        Ok(result)
    }

    fn apply(&self, value: [f32; 3]) -> [f32; 3] {
        let mut col = value.map(|v| v.max(0.) * self.exposure.exp2());
        if self.highlights > 0. {
            let t = 1. - 0.85 * self.highlights;
            let m = col[0].max(col[1]).max(col[2]);
            if m > t {
                let shoulder = t + (1. - t) * (1. - (-(m - t) / (1. - t)).exp());
                col = col.map(|v| v * shoulder / m.max(1e-6));
            }
        }
        if self.black != 0. || self.white != 1. || self.midtones != 1. {
            let span = (self.white - self.black).max(1e-5);
            col = col.map(|v| ((v - self.black) / span).max(0.));
            if self.midtones != 1. {
                col = col.map(|v| v.powf(self.midtones));
            }
        }
        col.map(|v| v.clamp(0., 1.))
    }
}

struct Brush {
    mask: Plane,
    gain: Plane,
    bias: Plane,
}

struct Layer {
    source: usize,
    mask: Option<usize>,
    mask_black: f32,
    mask_white: f32,
    mask_invert: bool,
    mode: String,
    blend: String,
    tint: [f32; 3],
    lut: Option<[[f32; 3]; 256]>,
    opacity: f32,
    tone: Tone,
    transform: Transform,
    clip: Option<Clip>,
    feather: f32,
    irregularity: f32,
    brush: Option<Brush>,
}

struct Source {
    key: String,
    metadata: Value,
    full: Plane,
    edge: Option<Plane>,
    wave: f32,
}

struct Project {
    sources: Vec<Source>,
    luminance: Option<Plane>,
    luminance_meta: Value,
    lum_edge: Option<Plane>,
    width: u32,
    height: u32,
    canvas_extent: [f64; 2],
    storage_origin: [f32; 2],
    brush_extent: [f64; 2],
    detail_scale: f32,
    file_region: Option<(u32, u32, crate::native::Region)>,
}

impl Project {
    fn load(folder: &Path, meta: &Value, cancelled: &dyn Fn() -> bool) -> Result<Self> {
        Self::load_region(folder, meta, None, cancelled)
    }

    fn load_region(
        folder: &Path,
        meta: &Value,
        region: Option<crate::native::Region>,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self> {
        let full = dimensions(meta)?;
        let canvas_extent = canvas_extent(meta)?;
        let mut storage_origin = crate::native::window_origin(meta)?;
        let file_region = region
            .map(|region| -> Result<_> {
                let x = region
                    .x
                    .checked_sub(storage_origin[0] as u32)
                    .context("Invalid source tile origin")?;
                let y = region
                    .y
                    .checked_sub(storage_origin[1] as u32)
                    .context("Invalid source tile origin")?;
                storage_origin = [region.x as f32, region.y as f32];
                Ok((full.0, full.1, crate::native::Region { x, y, ..region }))
            })
            .transpose()?;
        let (width, height) = region.map_or(full, |r| (r.width, r.height));
        let load = |path: &Path| -> Result<Plane> {
            match file_region {
                Some((w, h, part)) => Plane::load_region(path, (w, h), part, cancelled),
                None => Plane::load(path, width, height, cancelled),
            }
        };
        if canvas_extent
            .iter()
            .any(|v| !(*v as f32).is_finite() || *v as f32 <= 0.)
        {
            bail!("metadata _canvas_extent exceeds float32 sampling geometry range");
        }
        let detail_scale = scalar(meta, "_detail_scale", 1.)?;
        if detail_scale <= 0. {
            bail!("metadata _detail_scale must be positive");
        }
        let brush_scale = number(meta, "_detail_scale", 1.)?;
        let brush_extent = canvas_extent.map(|v| v / brush_scale);
        let layers = meta["layers"]
            .as_array()
            .context("metadata layers must be an array")?;
        if layers.is_empty() {
            bail!("project contains no science source planes");
        }
        let mut sources: Vec<Source> = Vec::new();
        for layer in layers {
            check_cancelled(cancelled)?;
            let key = text(layer, "key", "")?;
            if key.is_empty() || key == "lum" || sources.iter().any(|s| s.key == key) {
                bail!("invalid or duplicate science source key: {key:?}");
            }
            let path = project_file(folder, text(layer, "file", "")?)?;
            sources.push(Source {
                key: key.into(),
                metadata: layer.clone(),
                full: load(&path)?,
                edge: None,
                wave: log_position(number(layer, "wavelength_nm", 550.)?)? as f32,
            });
        }
        let luminance_meta = meta["luminance"].clone();
        let luminance = if luminance_meta.is_null() {
            None
        } else {
            Some(load(&project_file(
                folder,
                text(&luminance_meta, "file", "")?,
            )?)?)
        };
        Ok(Self {
            sources,
            luminance,
            luminance_meta,
            lum_edge: None,
            width,
            height,
            canvas_extent,
            storage_origin,
            brush_extent,
            detail_scale,
            file_region,
        })
    }

    fn index(&self, key: &str) -> Result<usize> {
        if key == "lum" {
            return Ok(self.sources.len());
        }
        self.sources
            .iter()
            .position(|s| s.key == key)
            .ok_or_else(|| anyhow::anyhow!("unknown science source: {key}"))
    }

    fn sample(&self, plane: &Plane, x: f32, y: f32) -> f32 {
        plane.sample_pixels(
            x * self.canvas_extent[0] as f32 - self.storage_origin[0],
            y * self.canvas_extent[1] as f32 - self.storage_origin[1],
        )
    }

    fn value(&self, source: usize, x: f32, y: f32) -> f32 {
        if source < self.sources.len() {
            self.sample(&self.sources[source].full, x, y)
        } else if let Some(lum) = &self.luminance {
            self.sample(lum, x, y)
        } else {
            // Exactly the shader's fallback: mean of full samples, not a star
            // residual, brightness-weighted mean, or mandatory luminance asset.
            self.sources
                .iter()
                .map(|s| self.sample(&s.full, x, y))
                .sum::<f32>()
                / self.sources.len() as f32
        }
    }

    fn coverage(
        &self,
        source: usize,
        folder: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<bool>> {
        let s = &self.sources[source];
        let default = format!("{}.coverage.u8", s.key);
        let path = project_file(folder, text(&s.metadata, "coverage_file", &default)?)?;
        let finite = s.full.finite_coverage();
        if path.is_file() {
            let mut covered = load_coverage(&path, self.width, self.height, cancelled)?;
            for (v, finite) in covered.iter_mut().zip(finite) {
                *v &= finite;
            }
            Ok(covered)
        } else {
            // Old builds have no coverage file: all finite science is covered,
            // including zeros/negative sky. Never infer footprint from brightness.
            Ok(finite)
        }
    }

    fn ensure_edge(
        &mut self,
        source: usize,
        folder: &Path,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<()> {
        let lum = source == self.sources.len();
        if if lum {
            self.lum_edge.is_some()
        } else {
            self.sources[source].edge.is_some()
        } {
            return Ok(());
        }
        let default = if lum {
            "luminance.edge.f16".into()
        } else {
            format!("{}.edge.f16", self.sources[source].key)
        };
        let metadata = if lum {
            &self.luminance_meta
        } else {
            &self.sources[source].metadata
        };
        let path = project_file(folder, text(metadata, "edge_file", &default)?)?;
        let edge = if path.is_file() {
            match self.file_region {
                Some((w, h, part)) => Plane::load_region(&path, (w, h), part, cancelled)?,
                None => Plane::load(&path, self.width, self.height, cancelled)?,
            }
        } else {
            if self.file_region.is_some() {
                bail!("Native tile is missing its prepared edge plane; rebuild preparation");
            }
            let mut covered = if lum {
                filled(pixel_count(self.width, self.height)?, false)?
            } else {
                self.coverage(source, folder, cancelled)?
            };
            if lum {
                for i in 0..self.sources.len() {
                    check_cancelled(cancelled)?;
                    let source_coverage = self.coverage(i, folder, cancelled)?;
                    for (v, add) in covered.iter_mut().zip(source_coverage) {
                        *v |= add;
                    }
                }
            }
            let distance = edge_distance_scaled(
                &covered,
                self.width,
                self.height,
                self.detail_scale,
                cancelled,
            )?;
            Plane::from_half_values(self.width, self.height, distance)?
        };
        if lum {
            self.lum_edge = Some(edge);
        } else {
            self.sources[source].edge = Some(edge);
        }
        Ok(())
    }

    fn distance(&self, source: usize, x: f32, y: f32) -> f32 {
        if source == self.sources.len() {
            self.sample(self.lum_edge.as_ref().unwrap(), x, y)
        } else {
            self.sample(self.sources[source].edge.as_ref().unwrap(), x, y)
        }
    }
}

impl Layer {
    fn parse(value: &Value, project: &Project, cancelled: &dyn Fn() -> bool) -> Result<Self> {
        fields(
            value,
            &[
                "id",
                "name",
                "visible",
                "source",
                "mode",
                "color",
                "gradient",
                "spectral",
                "black",
                "white",
                "stretch",
                "k",
                "invert",
                "curve",
                "detail",
                "clip",
                "intensity",
                "toneMatch",
                "opacity",
                "blend",
                "mask",
                "transform",
                "edge",
                "brush",
            ],
            "layer",
        )?;
        let mode = text(value, "mode", "tint")?;
        if !["tint", "gradient", "spectral"].contains(&mode) {
            bail!("unsupported colour mode: {mode}");
        }
        let blend = text(value, "blend", "screen")?;
        if !BLENDS.contains(&blend) {
            bail!("unsupported blend mode: {blend}");
        }
        fields(&value["spectral"], &["stops"], "spectral")?;
        fields(
            &value["mask"],
            &["source", "black", "white", "invert"],
            "layer mask",
        )?;
        fields(&value["edge"], &["feather", "irregularity"], "layer edge")?;
        let mask_key = text(&value["mask"], "source", "")?;
        let result = Self {
            source: project.index(text(value, "source", "lum")?)?,
            mask: if mask_key.is_empty() {
                None
            } else {
                Some(project.index(mask_key)?)
            },
            mask_black: scalar(&value["mask"], "black", 0.)?,
            mask_white: scalar(&value["mask"], "white", 1.)?,
            mask_invert: boolean(&value["mask"], "invert", false)?,
            mode: mode.into(),
            blend: blend.into(),
            tint: rgb(text(value, "color", "#dcd8cf")?)?,
            lut: match mode {
                "gradient" => Some(gradient_lut(&value["gradient"], false)?),
                "spectral" => Some(gradient_lut(&value["spectral"]["stops"], true)?),
                _ => None,
            },
            opacity: scalar(value, "opacity", 1.)?,
            tone: Tone::parse(value)?,
            transform: Transform::parse(&value["transform"])?,
            clip: Clip::parse(&value["clip"])?,
            feather: scalar(&value["edge"], "feather", 0.)?,
            irregularity: scalar(&value["edge"], "irregularity", 0.)?,
            brush: brush_mask(
                &value["brush"],
                project.brush_extent[0],
                project.brush_extent[1],
                cancelled,
            )?,
        };
        if !(0. ..=1.).contains(&result.opacity)
            || result.feather < 0.
            || !(0. ..=1.).contains(&result.irregularity)
        {
            bail!("opacity/edge irregularity must be 0..1 and edge feather nonnegative");
        }
        Ok(result)
    }

    fn signal(&self, project: &Project, source: usize, x: f32, y: f32) -> f32 {
        let value = project.value(source, x, y);
        if let Some(brush) = &self.brush {
            value * brush.gain.sample(x, y) + brush.bias.sample(x, y)
        } else {
            value
        }
    }
}

fn brush_mask(
    value: &Value,
    width: f64,
    height: f64,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<Brush>> {
    fields(value, &["strokes"], "brush")?;
    if value["strokes"].is_null() {
        return Ok(None);
    }
    let strokes = value["strokes"]
        .as_array()
        .context("brush strokes must be an array")?;
    if strokes.is_empty() {
        return Ok(None);
    }
    let scale = (width.max(height) / 1024.).ceil().max(1.);
    let mw = rounded_dimension((width / scale).ceil())?;
    let mh = rounded_dimension((height / scale).ceil())?;
    let (sx, sy) = (width / mw as f64, height / mh as f64);
    let mut mask = filled(pixel_count(mw, mh)?, 1_f32)?;
    let mut gain = filled(mask.len(), 1_f32)?;
    let mut bias = filled(mask.len(), 0_f32)?;
    let mut footprint = filled(mask.len(), 0_f32)?;
    for stroke in strokes {
        check_cancelled(cancelled)?;
        fields(
            stroke,
            &["mode", "size", "hardness", "strength", "points"],
            "brush stroke",
        )?;
        let mode = text(stroke, "mode", "hide")?;
        if !["hide", "reveal", "black", "white"].contains(&mode) {
            bail!("unsupported brush stroke mode: {mode}");
        }
        let size = number(stroke, "size", 1.)?;
        if size < 0. {
            bail!("brush size must be nonnegative");
        }
        let radius = (size / 2.).max(0.5);
        let spacing = (size * 0.15).max(1.);
        let hardness = number(stroke, "hardness", 1.)?.clamp(0., 1.);
        let strength = number(stroke, "strength", 1.)?.clamp(0., 1.);
        let points = stroke["points"]
            .as_array()
            .context("brush points must be an array")?;
        let mut previous: Option<[f64; 2]> = None;
        footprint.fill(0.);
        let mut dab_count = 0usize;
        let mut dab = |x: f64, y: f64| -> Result<()> {
            dab_count += 1;
            if dab_count % 64 == 1 {
                check_cancelled(cancelled)?;
            }
            let x0 = ((x - radius) / sx - 0.5).ceil().max(0.) as i64;
            let x1 = ((x + radius) / sx - 0.5).floor().min(mw as f64 - 1.) as i64;
            let y0 = ((y - radius) / sy - 0.5).ceil().max(0.) as i64;
            let y1 = ((y + radius) / sy - 0.5).floor().min(mh as f64 - 1.) as i64;
            for j in y0..=y1 {
                for i in x0..=x1 {
                    let r = ((i as f64 + 0.5) * sx - x).hypot((j as f64 + 0.5) * sy - y) / radius;
                    let t = ((r - hardness) / (1. - hardness).max(1e-12)).clamp(0., 1.);
                    let falloff = if r <= hardness {
                        1.
                    } else if r >= 1. {
                        0.
                    } else {
                        1. - t * t * (3. - 2. * t)
                    };
                    let index = j as usize * mw as usize + i as usize;
                    footprint[index] = footprint[index].max((falloff * strength) as f32);
                }
            }
            Ok(())
        };
        for point in points {
            let p = point.as_array().context("brush point must be [x,y]")?;
            if p.len() != 2 {
                bail!("brush point must be [x,y]");
            }
            let (x, y) = (
                p[0].as_f64().context("brush x must be finite")?,
                p[1].as_f64().context("brush y must be finite")?,
            );
            if !x.is_finite() || !y.is_finite() {
                bail!("brush point must be finite");
            }
            if let Some([ax, ay]) = previous {
                let steps = ((x - ax).hypot(y - ay) / spacing).ceil().max(1.);
                if !steps.is_finite() || steps > 10_000_000. {
                    bail!("brush segment requires too many dabs");
                }
                for j in 1..=steps as usize {
                    dab(
                        ax + (x - ax) * j as f64 / steps,
                        ay + (y - ay) * j as f64 / steps,
                    )?;
                }
            } else {
                dab(x, y)?;
            }
            previous = Some([x, y]);
        }
        for (i, f) in footprint.iter().enumerate() {
            match mode {
                "hide" => mask[i] *= 1. - f,
                "reveal" => mask[i] += (1. - mask[i]) * f,
                _ => {
                    gain[i] *= 1. - f;
                    bias[i] = bias[i] * (1. - f) + if mode == "white" { *f } else { 0. };
                }
            }
        }
    }
    // RGBA16F brush upload quantizes before bilinear sampling.
    for m in mask.iter_mut().chain(&mut gain).chain(&mut bias) {
        *m = half::f16::from_f32(m.clamp(0., 1.)).to_f32();
    }
    Ok(Some(Brush {
        mask: Plane::from_f32(mw, mh, mask)?,
        gain: Plane::from_f32(mw, mh, gain)?,
        bias: Plane::from_f32(mw, mh, bias)?,
    }))
}

fn gaussian_blur(
    input: &[f32],
    width: u32,
    height: u32,
    sigma: f32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let n = pixel_count(width, height)?;
    if input.len() != n || sigma <= 0. || !sigma.is_finite() {
        bail!("invalid detail Gaussian input");
    }
    let radius = (4. * sigma + 0.5).floor() as usize;
    if radius > 65536 {
        bail!("detail Gaussian radius exceeds supported sampling scale");
    }
    let mut kernel: Vec<f64> = (0..=2 * radius)
        .map(|i| {
            let x = (i as f64 - radius as f64) / sigma as f64;
            (-0.5 * x * x).exp()
        })
        .collect();
    let total: f64 = kernel.iter().sum();
    for weight in &mut kernel {
        *weight /= total;
    }
    let mut scratch = filled(n, 0_f32)?;
    let mut output = filled(n, 0_f32)?;
    let w = width as usize;
    // Cancellation stays on the calling thread, so the public callback need not
    // be Sync. Workers only see independent rows and immutable image/kernel data.
    for (batch, rows) in scratch.chunks_mut(w * 8).enumerate() {
        check_cancelled(cancelled)?;
        rows.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
            let y = batch * 8 + row;
            for (x, value) in out.iter_mut().enumerate() {
                let mut sum = 0_f64;
                for (i, weight) in kernel.iter().enumerate() {
                    let ix = (x as isize + i as isize - radius as isize).clamp(0, w as isize - 1)
                        as usize;
                    sum += input[y * w + ix] as f64 * weight;
                }
                *value = sum as f32;
            }
        });
    }
    for (batch, rows) in output.chunks_mut(w * 8).enumerate() {
        check_cancelled(cancelled)?;
        rows.par_chunks_mut(w).enumerate().for_each(|(row, out)| {
            let y = batch * 8 + row;
            for (x, value) in out.iter_mut().enumerate() {
                let mut sum = 0_f64;
                for (i, weight) in kernel.iter().enumerate() {
                    let iy = (y as isize + i as isize - radius as isize)
                        .clamp(0, height as isize - 1) as usize;
                    sum += scratch[iy * w + x] as f64 * weight;
                }
                *value = sum as f32;
            }
        });
    }
    Ok(output)
}

fn outside(x: f32, y: f32) -> bool {
    !(0. ..=1.).contains(&x) || !(0. ..=1.).contains(&y)
}

/// Render full science planes into top-down post-output-tone RGB.
///
/// `long_side` caps the final crop-frame dimensions without upscaling. Detail and
/// edge pixel units honor prepared-project `_detail_scale`; normalized geometry
/// uses optional unrounded `_canvas_extent`, otherwise the integer storage size.
/// Cancellation is checked during I/O, every image row, and blur.
pub fn render_project(
    project_dir: &Path,
    state: &Value,
    long_side: Option<u32>,
    cancelled: &dyn Fn() -> bool,
) -> Result<RenderedImage> {
    render_project_inner(project_dir, state, long_side, None, None, cancelled)
}

/// Sample a source-backed prepared canvas onto the planner's exact output lattice.
/// Dimensions change sampling density only, never the physical crop/sky frame.
pub(crate) fn render_project_with_dimensions(
    project_dir: &Path,
    state: &Value,
    dimensions: (u32, u32),
    cancelled: &dyn Fn() -> bool,
) -> Result<RenderedImage> {
    render_project_inner(project_dir, state, None, Some(dimensions), None, cancelled)
}

fn render_tiled(
    project_dir: &Path,
    state: &Value,
    dimensions: (u32, u32),
    tiles: &Value,
    cancelled: &dyn Fn() -> bool,
) -> Result<RenderedImage> {
    let tiles: Vec<crate::native::RenderTile> =
        serde_json::from_value(tiles.clone()).context("Invalid native output tiles")?;
    let (width, height) = dimensions;
    let mut pixels = filled(
        pixel_count(width, height)?
            .checked_mul(3)
            .context("output RGB buffer size overflow")?,
        0_f32,
    )?;
    for tile in tiles {
        check_cancelled(cancelled)?;
        let image = render_project_inner(
            project_dir,
            state,
            None,
            Some(dimensions),
            Some(tile),
            cancelled,
        )?;
        for row in 0..image.height {
            let destination = ((row + tile.output.y) * width + tile.output.x) as usize * 3;
            let source = row as usize * image.width as usize * 3;
            pixels[destination..destination + image.width as usize * 3]
                .copy_from_slice(&image.pixels[source..source + image.width as usize * 3]);
        }
    }
    check_cancelled(cancelled)?;
    Ok(RenderedImage {
        width,
        height,
        pixels,
    })
}

fn render_project_inner(
    project_dir: &Path,
    state: &Value,
    long_side: Option<u32>,
    planned_dimensions: Option<(u32, u32)>,
    tile: Option<crate::native::RenderTile>,
    cancelled: &dyn Fn() -> bool,
) -> Result<RenderedImage> {
    check_cancelled(cancelled)?;
    fields(
        state,
        &[
            "version",
            "layers",
            "selectedLayer",
            "global",
            "starOverrides",
            "starWeights",
            "starClasses",
            "starsMigration",
            "sources",
            "checkpoints",
            "revision",
            "rev",
        ],
        "editor state",
    )?;
    fields(
        &state["global"],
        &["crop", "rotation", "flipX", "stars", "parallax", "output"],
        "global",
    )?;
    fields(
        &state["global"]["crop"],
        &["x", "y", "w", "h", "rotation"],
        "global crop",
    )?;
    let output = Output::parse(&state["global"]["output"])?;
    let layers = state["layers"]
        .as_array()
        .context("editor state layers must be an array")?;
    let meta: Value = serde_json::from_reader(BufReader::new(
        File::open(project_dir.join("meta.json")).context("cannot read project meta.json")?,
    ))
    .context("invalid project meta.json")?;
    let view = View::new(&meta, state)?;
    let (ow, oh) = match planned_dimensions {
        Some((0, _)) | Some((_, 0)) => bail!("output dimensions must be positive"),
        Some(dimensions) => dimensions,
        None => view.output_dimensions(long_side)?,
    };
    if tile.is_none()
        && meta["_native_tiles"]
            .as_array()
            .is_some_and(|t| !t.is_empty())
    {
        return render_tiled(
            project_dir,
            state,
            (ow, oh),
            &meta["_native_tiles"],
            cancelled,
        );
    }
    let mut project = match tile {
        Some(tile) => Project::load_region(project_dir, &meta, Some(tile.source), cancelled)?,
        None => Project::load(project_dir, &meta, cancelled)?,
    };
    let region = match tile {
        Some(tile) => tile.render,
        None => crate::native::render_window(&meta)?,
    };
    let (width, height) = (region.width, region.height);
    let origin = [region.x as f32, region.y as f32];
    let extent = project.canvas_extent;
    let (w, h) = (extent[0] as f32, extent[1] as f32);
    // One accumulation and one layer's detail/brush buffers, never per-layer RGB
    // contributions or star copies. Cached science stays f16; temporary native
    // f32 planes retain their unquantized precision.
    let count = pixel_count(width, height)?;
    let mut acc = filled(
        count.checked_mul(3).context("RGB buffer size overflow")?,
        0_f32,
    )?;
    for value in layers.iter().rev() {
        if !boolean(value, "visible", true)? {
            continue;
        }
        check_cancelled(cancelled)?;
        let layer = Layer::parse(value, &project, cancelled)?;
        if layer.tone.needs_coverage() || layer.feather > 0. {
            project.ensure_edge(layer.source, project_dir, cancelled)?;
        }
        let blurred = if layer.tone.amount > 0. {
            let detail_source = if layer.mode == "spectral" {
                project.sources.len()
            } else {
                layer.source
            };
            let mut base = filled(count, 0_f32)?;
            for y in 0..height {
                check_cancelled(cancelled)?;
                for x in 0..width {
                    let (qx, qy) = layer.transform.point(
                        (x as f32 + origin[0] + 0.5) / w,
                        (y as f32 + origin[1] + 0.5) / h,
                        w,
                        h,
                    );
                    base[(y * width + x) as usize] = if outside(qx, qy) {
                        0.
                    } else {
                        layer
                            .tone
                            .base(layer.signal(&project, detail_source, qx, qy))
                            .clamp(0., 1.)
                    };
                }
            }
            Some(gaussian_blur(
                &base,
                width,
                height,
                layer.tone.radius * project.detail_scale,
                cancelled,
            )?)
        } else {
            None
        };
        for y in 0..height {
            check_cancelled(cancelled)?;
            for x in 0..width {
                let index = (y * width + x) as usize;
                let (cx, cy) = (
                    (x as f32 + origin[0] + 0.5) / w,
                    (y as f32 + origin[1] + 0.5) / h,
                );
                let (qx, qy) = layer.transform.point(cx, cy, w, h);
                if outside(qx, qy) {
                    continue;
                }
                let detail = blurred.as_ref().map(|v| v[index]);
                let v = layer
                    .tone
                    .apply(layer.signal(&project, layer.source, qx, qy), detail);
                let mut bright = v;
                let (hue, mut color) = if layer.mode == "gradient" {
                    let hue = sample_gradient(layer.lut.as_ref().unwrap(), v);
                    (hue, hue.map(|c| c * layer.tone.intensity))
                } else if layer.mode == "spectral" {
                    let (mut sum, mut wave) = (0_f32, 0_f32);
                    for source in &project.sources {
                        let val = project.sample(&source.full, qx, qy).max(0.);
                        sum += val;
                        wave += val * source.wave;
                    }
                    let t = if sum > 1e-6 { wave / sum } else { 0.5 };
                    let hue = sample_gradient(layer.lut.as_ref().unwrap(), t);
                    bright = layer.tone.apply(
                        layer.signal(&project, project.sources.len(), qx, qy),
                        detail,
                    );
                    (hue, hue.map(|c| c * bright))
                } else {
                    (layer.tint, layer.tint.map(|c| c * v))
                };
                let mut alpha = layer.opacity;
                // Layer clips remain in canvas space, independent of transforms.
                if let Some(clip) = &layer.clip {
                    alpha *= clip.alpha(cx, cy, w, h);
                }
                let painted = layer
                    .brush
                    .as_ref()
                    .map_or(0., |brush| 1. - brush.gain.sample(qx, qy));
                if layer.tone.needs_coverage() {
                    alpha *= painted.max(smooth(
                        project.distance(layer.source, qx, qy) / project.detail_scale,
                    ));
                }
                if let Some(brush) = &layer.brush {
                    alpha *= brush.mask.sample(qx, qy);
                }
                // Normal uses brightness as coverage, not brightness squared.
                if layer.blend == "normal" {
                    color = hue;
                    alpha *= bright;
                }
                if let Some(source) = layer.mask {
                    let mask = ((project.value(source, qx, qy) - layer.mask_black)
                        / (layer.mask_white - layer.mask_black).max(1e-5))
                    .clamp(0., 1.);
                    alpha *= if layer.mask_invert { 1. - mask } else { mask };
                }
                if layer.feather > 0. {
                    let feather = layer.feather * project.detail_scale;
                    let noise_scale = feather.max(project.detail_scale);
                    let noise = edge_noise(qx * w / noise_scale, qy * h / noise_scale);
                    let effective = feather * (1. + layer.irregularity * (noise - 0.5) * 1.6);
                    alpha *=
                        painted.max(smooth(project.distance(layer.source, qx, qy) / effective));
                }
                for c in 0..3 {
                    let before = acc[index * 3 + c];
                    acc[index * 3 + c] =
                        before + (blend_channel(before, color[c], &layer.blend) - before) * alpha;
                }
            }
        }
    }
    drop(project);
    check_cancelled(cancelled)?;
    let area = tile.map_or(
        crate::native::Region {
            x: 0,
            y: 0,
            width: ow,
            height: oh,
        },
        |t| t.output,
    );
    let out_count = pixel_count(area.width, area.height)?
        .checked_mul(3)
        .context("output RGB buffer size overflow")?;
    let mut pixels = filled(out_count, 0_f32)?;
    for y in area.y..area.y + area.height {
        check_cancelled(cancelled)?;
        for x in area.x..area.x + area.width {
            let ox = view.frame.x + (x as f64 + 0.5) / ow as f64 * view.frame.w;
            let oy = view.frame.y + (y as f64 + 0.5) / oh as f64 * view.frame.h;
            let (qx, qy) = view.image_point(ox, oy);
            let index = ((y - area.y) * area.width + x - area.x) as usize * 3;
            if !view.inside_crop(ox, oy) || qx < 0. || qx > 1. || qy < 0. || qy > 1. {
                // Match non-previewLinear editor export corners, not a fabricated
                // opaque black source pixel or contact-sheet panel background.
                pixels[index..index + 3].copy_from_slice(&[0.0784, 0.0824, 0.0902]);
                continue;
            }
            let color = if origin == [0., 0.] && extent == [width as f64, height as f64] {
                // Keep cached integer-canvas arithmetic bit-for-bit unchanged.
                sample_rgb(&acc, width, height, qx as f32, qy as f32)
            } else {
                sample_rgb_pixels(
                    &acc,
                    width,
                    height,
                    (qx * extent[0]) as f32 - origin[0],
                    (qy * extent[1]) as f32 - origin[1],
                )
            };
            pixels[index..index + 3].copy_from_slice(&output.apply(color));
        }
    }
    if pixels.iter().any(|v| !v.is_finite()) {
        bail!("non-finite rendered RGB; check active tone values");
    }
    check_cancelled(cancelled)?;
    Ok(RenderedImage {
        width: area.width,
        height: area.height,
        pixels,
    })
}

fn sample_rgb(data: &[f32], width: u32, height: u32, x: f32, y: f32) -> [f32; 3] {
    sample_rgb_pixels(data, width, height, x * width as f32, y * height as f32)
}

fn sample_rgb_pixels(data: &[f32], width: u32, height: u32, x: f32, y: f32) -> [f32; 3] {
    let px = (x - 0.5).clamp(0., width as f32 - 1.);
    let py = (y - 0.5).clamp(0., height as f32 - 1.);
    let (ix, iy) = (px.floor() as u32, py.floor() as u32);
    let (hx, hy) = ((ix + 1).min(width - 1), (iy + 1).min(height - 1));
    let (fx, fy) = (px - ix as f32, py - iy as f32);
    std::array::from_fn(|c| {
        let get = |x, y| data[(y * width + x) as usize * 3 + c];
        let (a, b, cc, d) = (get(ix, iy), get(hx, iy), get(ix, hy), get(hx, hy));
        (a + (b - a) * fx) * (1. - fy) + (cc + (d - cc) * fx) * fy
    })
}

/// Encode validated post-output RGB as png8, true RGB png16/tiff16, or JPEG.
/// No implicit gamma transform is applied; channels are clamped then rounded.
pub fn write_image(image: &RenderedImage, path: &Path, format: &str, quality: u8) -> Result<()> {
    validate_image(image, format, quality)?;
    if let Some(parent) = path.parent().filter(|v| !v.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).context("cannot create image output directory")?;
    }
    let file = File::create(path).with_context(|| format!("cannot write {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    encode_image(image, &mut writer, format, quality)?;
    writer.flush().context("cannot flush encoded image")?;
    Ok(())
}

pub(crate) fn encoded_size(image: &RenderedImage, format: &str, quality: u8) -> Result<u64> {
    validate_image(image, format, quality)?;
    let mut writer = Cursor::new(Vec::new());
    encode_image(image, &mut writer, format, quality)?;
    Ok(writer.get_ref().len() as u64)
}

fn validate_image(image: &RenderedImage, format: &str, quality: u8) -> Result<()> {
    let n = pixel_count(image.width, image.height)?
        .checked_mul(3)
        .context("RGB size overflow")?;
    if image.pixels.len() != n {
        bail!("invalid rendered RGB length (expected {n})");
    }
    if image.pixels.iter().any(|v| !v.is_finite()) {
        bail!("cannot encode non-finite RGB values");
    }
    if !["png8", "png16", "tiff16", "jpeg"].contains(&format) {
        bail!("unsupported image format: {format}");
    }
    if format == "jpeg" && !(1..=100).contains(&quality) {
        bail!("JPEG quality must be 1..100");
    }
    Ok(())
}

fn encode_image(
    image: &RenderedImage,
    mut writer: impl Write + Seek,
    format: &str,
    quality: u8,
) -> Result<()> {
    if matches!(format, "png8" | "jpeg") {
        let bytes: Vec<u8> = image
            .pixels
            .iter()
            .map(|v| (v.clamp(0., 1.) * 255.).round() as u8)
            .collect();
        if format == "png8" {
            image::codecs::png::PngEncoder::new(&mut writer).write_image(
                &bytes,
                image.width,
                image.height,
                ExtendedColorType::Rgb8,
            )?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut writer, quality).encode(
                &bytes,
                image.width,
                image.height,
                ExtendedColorType::Rgb8,
            )?;
        }
    } else {
        // image encoders expect native-endian u16 input and handle file endian.
        let bytes: Vec<u8> = image
            .pixels
            .iter()
            .flat_map(|v| ((v.clamp(0., 1.) * 65535.).round() as u16).to_ne_bytes())
            .collect();
        if format == "png16" {
            image::codecs::png::PngEncoder::new(&mut writer).write_image(
                &bytes,
                image.width,
                image.height,
                ExtendedColorType::Rgb16,
            )?;
        } else {
            image::codecs::tiff::TiffEncoder::new(&mut writer).write_image(
                &bytes,
                image.width,
                image.height,
                ExtendedColorType::Rgb16,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    fn fixture(values: &[f32], width: u32, height: u32) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("meta.json"),
            json!({
                "width": width, "height": height,
                "layers": [{"key": "a", "file": "a.f16", "wavelength_nm": 550}]
            })
            .to_string(),
        )
        .unwrap();
        let mut file = File::create(dir.path().join("a.f16")).unwrap();
        for value in values {
            file.write_all(&half::f16::from_f32(*value).to_bits().to_le_bytes())
                .unwrap();
        }
        dir
    }

    fn state() -> Value {
        json!({"layers": [{"source": "a", "color": "#ffffff", "stretch": "linear",
            "blend": "add", "mode": "tint"}], "global": {}})
    }

    #[test]
    fn no_star_assets_needed_full_science_orientation_flip_and_luminance_fallback() {
        let dir = fixture(&[0., 0.25, 0.5, 1.], 2, 2);
        let mut edits = state();
        edits["global"]["stars"] = json!({"separate": true, "brightness": 0});
        edits["starOverrides"] = json!({"1": {"gain": 0}});
        let image = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!((image.width, image.height), (2, 2));
        assert_eq!(
            image.pixels,
            vec![0., 0., 0., 0.25, 0.25, 0.25, 0.5, 0.5, 0.5, 1., 1., 1.]
        );
        edits["global"]["flipX"] = json!(true);
        edits["layers"][0]["source"] = json!("lum");
        let flipped = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!(&flipped.pixels[..6], &[0.25, 0.25, 0.25, 0., 0., 0.]);
        assert!(render_project(dir.path(), &edits, None, &|| true).is_err());
    }

    #[test]
    fn exact_blend_formulas_and_normal_brightness_coverage() {
        let expected = [0.8, 1.05, 0.85, 0.2, 0.4, 0.4, 0.8, 0.55, 0., 0.25];
        for (mode, expected) in BLENDS.iter().zip(expected) {
            assert!(
                (blend_channel(0.25, 0.8, mode) - expected).abs() < 1e-6,
                "{mode}"
            );
        }
        let dir = fixture(&[0.25], 1, 1);
        let mut edits = state();
        edits["layers"] = json!([
            {"source": "a", "color": "#ff0000", "stretch": "linear", "blend": "normal"},
            {"source": "a", "color": "#0000ff", "stretch": "linear", "blend": "add"}
        ]);
        let image = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!(image.pixels, vec![0.25, 0., 0.1875]);
    }

    #[test]
    fn curve_shape_and_identity_preserve_above_white_intensity_behavior() {
        let lut = curve_lut(&[(0., 0.1), (0.3, 0.7), (0.6, 0.7), (1., 0.9)]);
        assert_eq!(lut[0], 0.1);
        assert_eq!(lut[255], 0.9);
        assert!(lut.windows(2).all(|v| v[1] >= v[0]));
        assert!(lut[77..153].iter().all(|v| *v == 0.7));
        let dir = fixture(&[2.], 1, 1);
        let mut edits = state();
        edits["layers"][0]["intensity"] = json!(0.4);
        edits["layers"][0]["curve"] = json!([{"x": 0.4, "y": 0.4}]);
        assert_eq!(
            render_project(dir.path(), &edits, None, &|| false)
                .unwrap()
                .pixels,
            vec![0.8; 3]
        );
        edits["layers"][0]["invert"] = json!(true);
        assert_eq!(
            render_project(dir.path(), &edits, None, &|| false)
                .unwrap()
                .pixels,
            vec![0.; 3]
        );
    }

    #[test]
    fn gradient_has_two_distinct_intensity_uses_and_source_masks_sample_raw_values() {
        let dir = fixture(&[0.5], 1, 1);
        let mut edits = state();
        edits["layers"][0]["mode"] = json!("gradient");
        edits["layers"][0]["gradient"] = json!([
            {"t": 0, "color": "#000000"}, {"t": 1, "color": "#ffffff"}
        ]);
        edits["layers"][0]["intensity"] = json!(0.5);
        let image = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert!((image.pixels[0] - 0.125).abs() < 1e-7);
        edits["layers"][0]["mask"] = json!({"source": "a", "black": 0.25, "white": 0.75});
        assert!(
            (render_project(dir.path(), &edits, None, &|| false)
                .unwrap()
                .pixels[0]
                - 0.0625)
                .abs()
                < 1e-7
        );
    }

    #[test]
    fn detail_gaussian_nearest_kernel_and_noise_hash_are_contractual() {
        let blurred = gaussian_blur(&[0., 0., 1., 0., 0.], 5, 1, 1., &|| false).unwrap();
        assert!((blurred[2] - 0.39894348).abs() < 1e-7);
        assert!((blurred[1] - 0.24197145).abs() < 1e-7);
        assert_eq!(edge_hash(0, 0), 0.);
        assert!((edge_hash(1, 0) - 29142. / 65535.).abs() < 1e-7);
        assert!((edge_noise(0.37, -0.81) - 0.549_193_7).abs() < 1e-7);
        let clip = Clip {
            x: 0.1,
            y: 0.2,
            w: 0.7,
            h: 0.6,
            feather: 0.2,
            ellipse: true,
        };
        assert_eq!(clip.alpha(0.45, 0.5, 120., 80.), 1.);
        assert_eq!(clip.alpha(0., 0., 120., 80.), 0.);
    }

    #[test]
    fn manual_output_tone_follows_composition_and_exposure_and_keeps_legacy_looks() {
        let out =
            Output::parse(&json!({"exposure":1,"black":0.25,"white":1.75,"midtones":2})).unwrap();
        for (actual, expected) in
            out.apply([0.125, 0.375, 0.75])
                .into_iter()
                .zip([0., 1. / 9., 25. / 36.])
        {
            assert!(
                (actual - expected).abs() <= 2. * f32::EPSILON,
                "{actual} vs {expected}"
            );
        }
        let dir = fixture(&[0.25], 1, 1);
        let mut edits = state();
        let second = edits["layers"][0].clone();
        edits["layers"].as_array_mut().unwrap().push(second);
        let legacy = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!(legacy.pixels, vec![0.5; 3]);
        edits["global"]["output"] = json!({"black":0,"white":1,"midtones":1});
        assert_eq!(
            render_project(dir.path(), &edits, None, &|| false)
                .unwrap()
                .pixels,
            legacy.pixels
        );
        edits["global"]["output"] = json!({"black":0.25,"white":0.75,"midtones":2});
        assert_eq!(
            render_project(dir.path(), &edits, None, &|| false)
                .unwrap()
                .pixels,
            vec![0.25; 3]
        );
        let collapsed = Output::parse(&json!({"black":0.5,"white":0.5,"midtones":0.7})).unwrap();
        assert_eq!(collapsed.apply([0.49, 0.5, 0.51]), [0., 0., 1.]);
        for invalid in [
            json!({"white":0}),
            json!({"black":-0.1}),
            json!({"midtones":0}),
            json!({"black":true}),
            json!({"white":"bad"}),
        ] {
            assert!(Output::parse(&invalid).is_err());
        }
    }

    #[test]
    fn output_rolloff_preserves_rgb_ratios_unknown_active_controls_are_errors() {
        let out = Output {
            exposure: 1.,
            highlights: 0.8,
            black: 0.,
            white: 1.,
            midtones: 1.,
        };
        let rgb = out.apply([1., 0.5, 0.25]);
        assert!(rgb[0] < 1.);
        assert!((rgb[1] / rgb[0] - 0.5).abs() < 1e-7);
        assert!((rgb[2] / rgb[0] - 0.25).abs() < 1e-7);
        let dir = fixture(&[0.5], 1, 1);
        let mut edits = state();
        edits["global"]["output"]["shadows"] = json!(0.5);
        assert!(
            render_project(dir.path(), &edits, None, &|| false)
                .err()
                .unwrap()
                .to_string()
                .contains("shadows")
        );
    }

    #[test]
    fn source_paint_fills_missing_coverage_and_keeps_layer_tone_and_colour() {
        let dir = fixture(
            &[f32::NAN, 0.2, 0.2, 0.2, f32::NAN, 0.2, 0.2, 0.2, 0.2],
            3,
            3,
        );
        let mut edits = state();
        let layer = &mut edits["layers"][0];
        layer["color"] = json!("#804020");
        layer["blend"] = json!("normal");
        layer["intensity"] = json!(0.5);
        layer["opacity"] = json!(0.7);
        layer["curve"] = json!([{"x":0,"y":0.2},{"x":1,"y":1}]);
        layer["brush"] = json!({"strokes":[{
            "mode":"white","size":1,"hardness":1,"strength":1,"points":[[1.5,1.5]]
        }]});
        let image = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!(&image.pixels[..3], &[0.; 3]);
        for (channel, tint) in rgb("#804020").unwrap().into_iter().enumerate() {
            assert!((image.pixels[12 + channel] - tint * 0.35).abs() < 1e-6);
        }
        edits["layers"][0]["brush"]["strokes"][0]["mode"] = json!("black");
        let darkened = render_project(dir.path(), &edits, None, &|| false).unwrap();
        for (channel, tint) in rgb("#804020").unwrap().into_iter().enumerate() {
            assert!((darkened.pixels[12 + channel] - tint * 0.07).abs() < 1e-6);
        }
    }

    #[test]
    fn brush_dabs_max_combine_within_strokes_and_reveal_respects_order() {
        // Design decision: repeated overlapping dabs in one stroke do not build
        // opacity; separate strokes do, and reveal restores the existing mask.
        let hide = json!({"mode": "hide", "size": 2, "hardness": 1, "strength": 0.5,
            "points": [[1.5, 1.5], [1.5, 1.5]]});
        let reveal = json!({"mode": "reveal", "size": 2, "hardness": 1, "strength": 0.5,
            "points": [[1.5, 1.5]]});
        let once = brush_mask(&json!({"strokes": [hide.clone()]}), 3., 3., &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(once.mask.sample(0.5, 0.5), 0.5);
        let twice = brush_mask(
            &json!({"strokes": [hide.clone(), hide.clone()]}),
            3.,
            3.,
            &|| false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(twice.mask.sample(0.5, 0.5), 0.25);
        let restored = brush_mask(&json!({"strokes": [hide, reveal]}), 3., 3., &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(restored.mask.sample(0.5, 0.5), 0.75);
        assert_eq!(restored.mask.sample(1. / 6., 1. / 6.), 1.);
        let white = json!({"mode":"white","size":2,"hardness":1,"strength":0.5,
            "points":[[1.5,1.5],[1.5,1.5]]});
        let black = json!({"mode":"black","size":2,"hardness":1,"strength":0.5,
            "points":[[1.5,1.5]]});
        let painted = brush_mask(&json!({"strokes":[white,black]}), 3., 3., &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(painted.mask.sample(0.5, 0.5), 1.);
        assert_eq!(painted.gain.sample(0.5, 0.5), 0.25);
        assert_eq!(painted.bias.sample(0.5, 0.5), 0.25);
    }

    #[test]
    fn layer_clip_inherited_rotation_is_inert_but_global_crop_anchor_is_active() {
        // Design decision: LayerClip inherits Crop.rotation as inert metadata.
        // Python clip_coverage and GPU clipAlpha use only canvas-space geometry;
        // rotating the clip or rejecting saved metadata would change old looks.
        let dir = fixture(&[0.5; 48], 8, 6);
        for shape in ["rect", "ellipse"] {
            let mut edits = state();
            edits["layers"][0]["clip"] = json!({
                "x": 0.125, "y": 0.125, "w": 0.5, "h": 0.5,
                "feather": 0.2, "shape": shape
            });
            edits["layers"][0]["transform"] = json!({
                "x": 0.05, "y": -0.03, "scale": 1.15, "rotation": 17
            });
            let baseline = render_project(dir.path(), &edits, None, &|| false).unwrap();
            assert!(baseline.pixels.iter().any(|v| *v > 0.));
            assert!(baseline.pixels.contains(&0.));
            for rotation in [0., 37., -90., 360.] {
                edits["layers"][0]["clip"]["rotation"] = json!(rotation);
                let actual = render_project(dir.path(), &edits, None, &|| false).unwrap();
                assert_eq!(actual.pixels, baseline.pixels, "{shape}: {rotation}");
            }
            edits["global"] = json!({
                "rotation": 113,
                "crop": {"x": 0.1, "y": 0.2, "w": 0.5, "h": 0.5, "rotation": 23}
            });
            let anchored = render_project(dir.path(), &edits, None, &|| false).unwrap();
            assert_eq!((anchored.width, anchored.height), (3, 4));
            edits["global"]["crop"]["rotation"] = json!(113);
            let reanchored = render_project(dir.path(), &edits, None, &|| false).unwrap();
            assert_eq!((reanchored.width, reanchored.height), (4, 3));
        }
    }

    #[test]
    fn prepared_detail_scale_scales_gaussian_and_edge_units_not_geometry() {
        let values: Vec<f32> = (0..48)
            .map(|i| if i % 8 < 4 { 0.25 } else { 0.75 })
            .collect();
        let dir = fixture(&values, 8, 6);
        let meta_path = dir.path().join("meta.json");
        let mut meta: Value = serde_json::from_reader(File::open(&meta_path).unwrap()).unwrap();
        meta["_detail_scale"] = json!(2);
        std::fs::write(&meta_path, meta.to_string()).unwrap();
        let mut edits = state();
        edits["layers"][0]["detail"] = json!({"amount": 0.75, "radius": 2});
        edits["layers"][0]["edge"] = json!({"feather": 900, "irregularity": 0});
        let expected_blur = gaussian_blur(&values, 8, 6, 4., &|| false).unwrap();
        let image = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!((image.width, image.height), (8, 6));
        for ((rgb, v), blurred) in image
            .pixels
            .as_chunks::<3>()
            .0
            .iter()
            .zip(values)
            .zip(expected_blur)
        {
            let expected = (v + 0.75 * (v - blurred)).clamp(0., 1.);
            assert!((rgb[0] - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn fractional_native_canvas_samples_physical_pixels_and_keeps_integer_storage() {
        // Design decision: normalized geometry maps to unrounded canvas pixels,
        // not to the rounded array dimensions. Otherwise native exports shift.
        let values: Vec<f32> = (0..8).map(|i| (i % 4) as f32 / 3.6 + 0.5 / 3.6).collect();
        let dir = fixture(&values, 4, 2);
        let meta_path = dir.path().join("meta.json");
        let mut meta: Value = serde_json::from_reader(File::open(&meta_path).unwrap()).unwrap();
        meta["_canvas_extent"] = json!([3.6, 1.8]);
        meta["layers"][0]["file"] = json!("native.f32");
        std::fs::write(
            dir.path().join("native.f32"),
            values
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        std::fs::write(&meta_path, meta.to_string()).unwrap();
        let check = |image: &RenderedImage, expected: &[f32]| {
            assert_eq!(image.pixels.len(), expected.len() * 3);
            for (rgb, expected) in image.pixels.as_chunks::<3>().0.iter().zip(expected) {
                for value in rgb {
                    assert!((*value - expected).abs() < 1e-6, "{value} != {expected}");
                }
            }
        };
        let mut edits = state();
        let image = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!((image.width, image.height), (4, 2));
        check(
            &image,
            &[
                0.5 / 3.6,
                0.375,
                0.625,
                0.875,
                0.5 / 3.6,
                0.375,
                0.625,
                0.875,
            ],
        );
        edits["global"]["flipX"] = json!(true);
        check(
            &render_project(dir.path(), &edits, None, &|| false).unwrap(),
            &[
                0.875,
                0.625,
                0.375,
                0.5 / 3.6,
                0.875,
                0.625,
                0.375,
                0.5 / 3.6,
            ],
        );
        edits["global"] = json!({"crop": {"x": 0.1, "y": 0, "w": 0.65, "h": 1}});
        let cropped = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!((cropped.width, cropped.height), (2, 2));
        check(&cropped, &[0.2625, 0.5875, 0.2625, 0.5875]);
        edits["global"] = json!({});
        edits["layers"][0]["transform"] = json!({"x": 0.125, "scale": 0.8});
        check(
            &render_project(dir.path(), &edits, None, &|| false).unwrap(),
            &[0., 0.203_645_83, 0.5, 0.8125, 0., 0.203_645_83, 0.5, 0.8125],
        );
        edits["layers"][0]["transform"] = json!({});
        edits["layers"][0]["clip"] = json!({
            "x": 0.4, "y": 0, "w": 0.6, "h": 1, "feather": 0, "shape": "rect"
        });
        check(
            &render_project(dir.path(), &edits, None, &|| false).unwrap(),
            &[
                0.,
                0.354_166_66,
                0.625,
                0.875,
                0.,
                0.354_166_66,
                0.625,
                0.875,
            ],
        );

        let project = Project::load(dir.path(), &meta, &|| false).unwrap();
        assert_eq!((project.width, project.height), (4, 2));
        assert!((project.value(0, 0.5, 0.5) - 0.5).abs() < 1e-6);
        edits["layers"][0]["brush"] = json!({"strokes": [{
            "mode": "hide", "size": 1.6, "hardness": 0.3, "strength": 0.7,
            "points": [[1.2, 0.6]]
        }]});
        let layer = Layer::parse(&edits["layers"][0], &project, &|| false).unwrap();
        let expected = brush_mask(&edits["layers"][0]["brush"], 3.6, 1.8, &|| false)
            .unwrap()
            .unwrap();
        assert_eq!(
            layer.brush.as_ref().unwrap().mask.sample(0.125, 0.25),
            expected.mask.sample(0.125, 0.25)
        );
    }

    #[test]
    fn inversion_does_not_invent_exterior_nan_coverage_and_cancellation_is_mid_work() {
        let dir = fixture(&[f32::NAN, 0., -0.25, 0.5, 1., 0.75], 3, 2);
        let mut edits = state();
        edits["layers"][0]["invert"] = json!(true);
        let image = render_project(dir.path(), &edits, None, &|| false).unwrap();
        assert_eq!(&image.pixels[..3], &[0.; 3]);
        assert_eq!(&image.pixels[3..6], &[1.; 3]);
        assert_eq!(&image.pixels[6..9], &[1.; 3]);
        let calls = std::cell::Cell::new(0);
        let cancellation = || {
            calls.set(calls.get() + 1);
            calls.get() > 5
        };
        assert!(
            render_project(dir.path(), &edits, None, &cancellation)
                .err()
                .unwrap()
                .to_string()
                .contains("cancelled")
        );
    }

    #[test]
    fn writers_roundtrip_true_rgb16_and_fail_before_creating_invalid_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let image = RenderedImage {
            width: 2,
            height: 1,
            pixels: vec![0., 0.12345, 1., 0.5, 0.2, 0.7],
        };
        for format in ["png8", "png16", "tiff16", "jpeg"] {
            let path = dir.path().join(format);
            write_image(&image, &path, format, 91).unwrap();
            let read = image::ImageReader::open(&path)
                .unwrap()
                .with_guessed_format()
                .unwrap()
                .decode()
                .unwrap();
            assert_eq!((read.width(), read.height()), (2, 1));
            if matches!(format, "png16" | "tiff16") {
                assert_eq!(read.color(), image::ColorType::Rgb16);
                assert_eq!(
                    read.to_rgb16().as_raw()[1],
                    (0.12345_f32 * 65535.).round() as u16
                );
            }
        }
        let bad = RenderedImage {
            width: 2,
            height: 1,
            pixels: vec![0.; 3],
        };
        let path = dir.path().join("invalid");
        assert!(write_image(&bad, &path, "png8", 90).is_err());
        assert!(!path.exists());
        assert!(write_image(&image, &path, "fits", 90).is_err());
        assert!(write_image(&image, &path, "jpeg", 0).is_err());
    }

    #[test]
    fn supplied_python_oracles_match_float32_non_star_rendering() {
        let Some(folder) = std::env::var_os("ASTROIMP_RENDER_ORACLE") else {
            return;
        };
        let folder = std::path::PathBuf::from(folder);
        let reference: Value =
            serde_json::from_reader(File::open(folder.join("reference/render.json")).unwrap())
                .unwrap();
        let project = folder.join("data/sessions/rust-validation/project");
        for (name, case) in reference.as_object().unwrap() {
            let image = render_project(&project, &case["state"], None, &|| false).unwrap();
            assert_eq!(image.width, case["width"].as_u64().unwrap() as u32);
            assert_eq!(image.height, case["height"].as_u64().unwrap() as u32);
            let expected = case["pixels"].as_array().unwrap();
            assert_eq!(image.pixels.len(), expected.len());
            let peak = image
                .pixels
                .iter()
                .zip(expected)
                .map(|(a, b)| (*a as f64 - b.as_f64().unwrap()).abs())
                .fold(0_f64, f64::max);
            println!("Python oracle {name}: max float32 error {peak:.8}");
            assert!(peak < 2e-5, "Python oracle {name}: max error {peak}");
            let png = folder.join(format!("reference/python-{name}.png"));
            if png.is_file() {
                let expected = image::ImageReader::open(&png)
                    .unwrap()
                    .decode()
                    .unwrap()
                    .to_rgb8();
                let directory = tempfile::tempdir().unwrap();
                let path = directory.path().join("rust.png");
                write_image(&image, &path, "png8", 90).unwrap();
                let encoded = image::ImageReader::open(path)
                    .unwrap()
                    .decode()
                    .unwrap()
                    .to_rgb8();
                assert_eq!(encoded.dimensions(), expected.dimensions());
                let peak_byte = encoded
                    .as_raw()
                    .iter()
                    .zip(expected.as_raw())
                    .map(|(a, b)| a.abs_diff(*b))
                    .max()
                    .unwrap();
                println!("Python cached export {name}: max byte error {peak_byte}");
                assert!(
                    peak_byte <= 1,
                    "Python cached export {name}: byte error {peak_byte}"
                );
            }
        }
    }
}
