//! Anchored canvas crop/rotation/flip geometry, mirrored from `astroimp/view.py`.
use anyhow::{Result, bail};
use serde_json::Value;

pub(crate) fn number(value: &Value, key: &str, default: f64) -> Result<f64> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() => Ok(n),
            _ => bail!("{key} must be a finite number"),
        },
    }
}

pub(crate) fn boolean(value: &Value, key: &str, default: bool) -> Result<bool> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("{key} must be a boolean")),
    }
}

pub(crate) fn dimensions(meta: &Value) -> Result<(u32, u32)> {
    let dim = |key: &str| -> Result<u32> {
        let n = meta[key]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("metadata {key} must be a positive integer"))?;
        if n == 0 || n > u32::MAX as u64 {
            bail!("metadata {key} is outside the supported range");
        }
        Ok(n as u32)
    };
    Ok((dim("width")?, dim("height")?))
}

/// Unrounded canvas geometry in prepared-project pixel units. This optional
/// temporary metadata never changes the integer on-disk plane dimensions.
pub(crate) fn canvas_extent(meta: &Value) -> Result<[f64; 2]> {
    let (width, height) = dimensions(meta)?;
    let Some(value) = meta.get("_canvas_extent") else {
        return Ok([width as f64, height as f64]);
    };
    let values = value
        .as_array()
        .filter(|v| v.len() == 2)
        .ok_or_else(|| anyhow::anyhow!("metadata _canvas_extent must be [width, height]"))?;
    let mut extent = [0.; 2];
    for (out, value) in extent.iter_mut().zip(values) {
        *out = value
            .as_f64()
            .filter(|v| v.is_finite() && *v > 0.)
            .ok_or_else(|| {
                anyhow::anyhow!("metadata _canvas_extent values must be positive finite numbers")
            })?;
    }
    Ok(extent)
}

pub(crate) fn rounded_dimension(value: f64) -> Result<u32> {
    // floor(x+.5), not f64::round: the addition rounds away sub-ULP residue
    // at decimal crop half-pixels exactly like Python and JavaScript.
    let rounded = (value + 0.5).floor();
    if !value.is_finite() || value <= 0. || rounded > u32::MAX as f64 {
        bail!("view dimensions are outside the supported range");
    }
    Ok(rounded.max(1.) as u32)
}

#[derive(Clone, Copy)]
pub(crate) struct Frame {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub width: f64,
    pub height: f64,
}

pub(crate) struct View {
    pub frame: Frame,
    crop: [f64; 4],
    width: f64,
    height: f64,
    c: f64,
    s: f64,
    dc: f64,
    ds: f64,
    delta: f64,
    flip: bool,
}

impl View {
    pub fn new(meta: &Value, state: &Value) -> Result<Self> {
        if !state.is_object() {
            bail!("editor state must be an object");
        }
        let [w, h] = canvas_extent(meta)?;
        let g = &state["global"];
        let crop = &g["crop"];
        if (!g.is_null() && !g.is_object()) || (!crop.is_null() && !crop.is_object()) {
            bail!("global state and crop must be objects");
        }
        let x = number(crop, "x", 0.)?;
        let y = number(crop, "y", 0.)?;
        let cw = number(crop, "w", 1.)?;
        let ch = number(crop, "h", 1.)?;
        if cw <= 0. || ch <= 0. {
            bail!("crop width and height must be positive");
        }
        let rotation = number(g, "rotation", 0.)?;
        let full = x == 0. && y == 0. && cw == 1. && ch == 1.;
        let anchor = if full {
            rotation
        } else {
            number(crop, "rotation", rotation)?
        };
        let delta = rotation - anchor;
        let (ds, dc) = delta.to_radians().sin_cos();
        let mut frame = Frame {
            x,
            y,
            w: cw,
            h: ch,
            width: w * cw,
            height: h * ch,
        };
        if delta != 0. {
            // Snap quarter-turn floating-point residue exactly as Python/TypeScript do.
            let ac = if dc.abs() < 1e-12 { 0. } else { dc.abs() };
            let as_ = if ds.abs() < 1e-12 { 0. } else { ds.abs() };
            let fw = w * cw * ac + h * ch * as_;
            let fh = w * cw * as_ + h * ch * ac;
            let cx = (x + cw / 2. - 0.5) * w;
            let cy = (y + ch / 2. - 0.5) * h;
            frame = Frame {
                x: 0.5 + (dc * cx - ds * cy - fw / 2.) / w,
                y: 0.5 + (ds * cx + dc * cy - fh / 2.) / h,
                w: fw / w,
                h: fh / h,
                width: fw,
                height: fh,
            };
        }
        rounded_dimension(frame.width)?;
        rounded_dimension(frame.height)?;
        let (s, c) = rotation.to_radians().sin_cos();
        Ok(Self {
            frame,
            crop: [x, y, cw, ch],
            width: w,
            height: h,
            c,
            s,
            dc,
            ds,
            delta,
            flip: boolean(g, "flipX", false)?,
        })
    }

    /// Output/theta-space normalized coordinate to top-down source coordinate.
    pub fn image_point(&self, x: f64, y: f64) -> (f64, f64) {
        let px = (x - 0.5) * self.width;
        let py = (y - 0.5) * self.height;
        let mut qx = (self.c * px + self.s * py) / self.width;
        let qy = (-self.s * px + self.c * py) / self.height;
        if self.flip {
            qx = -qx;
        }
        (qx + 0.5, qy + 0.5)
    }

    pub fn inside_crop(&self, x: f64, y: f64) -> bool {
        if self.delta == 0. {
            return true;
        }
        let px = (x - 0.5) * self.width;
        let py = (y - 0.5) * self.height;
        let qx = (self.dc * px + self.ds * py) / self.width + 0.5;
        let qy = (-self.ds * px + self.dc * py) / self.height + 0.5;
        qx >= self.crop[0] - 1e-12
            && qx <= self.crop[0] + self.crop[2] + 1e-12
            && qy >= self.crop[1] - 1e-12
            && qy <= self.crop[1] + self.crop[3] + 1e-12
    }

    pub fn output_dimensions(&self, long_side: Option<u32>) -> Result<(u32, u32)> {
        let scale = match long_side {
            Some(0) => bail!("long_side must be positive"),
            Some(side) => (side as f64 / self.frame.width.max(self.frame.height)).min(1.),
            None => 1.,
        };
        Ok((
            rounded_dimension(self.frame.width * scale)?,
            rounded_dimension(self.frame.height * scale)?,
        ))
    }
}

/// Dimensions of the anchored crop's bounding frame, rounded like the editor.
pub fn view_dimensions(meta: &Value, state: &Value) -> Result<(u32, u32)> {
    View::new(meta, state)?.output_dimensions(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anchored_geometry_matches_shared_python_and_typescript_vectors() {
        let cases: Value =
            serde_json::from_str(include_str!("../../tests/view_cases.json")).unwrap();
        for case in cases.as_array().unwrap() {
            for flip in [false, true] {
                let mut global = case["g"].clone();
                global["flipX"] = json!(flip);
                let view = View::new(&case["size"], &json!({"global": global})).unwrap();
                for (actual, key) in [
                    (view.frame.x, "x"),
                    (view.frame.y, "y"),
                    (view.frame.w, "w"),
                    (view.frame.h, "h"),
                    (view.frame.width, "width"),
                    (view.frame.height, "height"),
                ] {
                    assert!((actual - case["frame"][key].as_f64().unwrap()).abs() < 1e-12);
                }
                for probe in case["probes"].as_array().unwrap() {
                    let x = probe["output"]["x"].as_f64().unwrap();
                    let y = probe["output"]["y"].as_f64().unwrap();
                    let (qx, qy) = view.image_point(x, y);
                    let ix = probe["image"]["x"].as_f64().unwrap();
                    assert!((qx - if flip { 1. - ix } else { ix }).abs() < 1e-12);
                    assert!((qy - probe["image"]["y"].as_f64().unwrap()).abs() < 1e-12);
                    assert_eq!(view.inside_crop(x, y), probe["inside"].as_bool().unwrap());
                }
            }
        }
    }

    #[test]
    fn full_crop_ignores_anchor_and_rounds_half_up() {
        // Design decision: rotating the uncropped canvas keeps its original frame.
        // Only a non-full anchored crop grows empty corners after delta rotation.
        let meta = json!({"width": 1000, "height": 600});
        let state = json!({"global": {"rotation": 37, "crop": {
            "x": 0, "y": 0, "w": 1, "h": 1, "rotation": -20
        }}});
        assert_eq!(view_dimensions(&meta, &state).unwrap(), (1000, 600));
        assert_eq!(
            view_dimensions(
                &json!({"width": 8, "height": 8}),
                &json!({"global": {
                    // Binary-exact 2.5 and 3.5 pixels; decimal .5005*1000 is
                    // slightly below 500.5 in Python/JavaScript as well.
                    "crop": {"x": 0, "y": 0, "w": 0.3125, "h": 0.4375}
                }})
            )
            .unwrap(),
            (3, 4)
        );
        assert!(view_dimensions(&meta, &json!({"global": {"crop": {"w": 0}}})).is_err());
    }

    #[test]
    fn temporary_fractional_canvas_extent_controls_frame_not_storage_dimensions() {
        // Design decision: native reprojection storage is rounded, but geometry
        // uses the unrounded original-canvas size multiplied by the native factor.
        let meta = json!({"width": 4, "height": 2, "_canvas_extent": [3.6, 1.8]});
        assert_eq!(dimensions(&meta).unwrap(), (4, 2));
        assert_eq!(canvas_extent(&meta).unwrap(), [3.6, 1.8]);
        assert_eq!(
            view_dimensions(&meta, &json!({"global": {"crop": {"w": 0.65}}})).unwrap(),
            (2, 2)
        );
        let state = json!({"global": {
            "rotation": 113,
            "crop": {"x": 0.1, "y": 0.2, "w": 0.65, "h": 0.5, "rotation": 23}
        }});
        let view = View::new(&meta, &state).unwrap();
        assert!((view.frame.width - 0.9).abs() < 1e-12);
        assert!((view.frame.height - 2.34).abs() < 1e-12);
        assert_eq!(view_dimensions(&meta, &state).unwrap(), (1, 2));
        assert_eq!(
            canvas_extent(&json!({"width": 4, "height": 2})).unwrap(),
            [4., 2.]
        );
        for invalid in [
            json!(null),
            json!([]),
            json!([1]),
            json!([1, 2, 3]),
            json!([0, 2]),
            json!([-1, 2]),
            json!(["3.6", 2]),
        ] {
            let mut bad = meta.clone();
            bad["_canvas_extent"] = invalid;
            assert!(View::new(&bad, &state).is_err());
        }
    }
}
