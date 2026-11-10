use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::sky::SkyWcs;

pub fn from_meta(meta: &Value) -> Value {
    let mut grid = serde_json::Map::new();
    for key in ["width", "height", "wcs", "pixel_scale_arcsec", "region_sky"] {
        if let Some(value) = meta.get(key) {
            grid.insert(key.into(), value.clone());
        }
    }
    Value::Object(grid)
}

pub fn same(old: &Value, new: &Value) -> bool {
    ["width", "height", "wcs"]
        .iter()
        .all(|key| old[*key] == new[*key])
}

pub fn view_grid(view: &Value, native_scale: f64, max_side: u32) -> Result<Value> {
    let width = view["field_arcmin"][0]
        .as_f64()
        .context("Missing captured field width")?
        * 60.;
    let height = view["field_arcmin"][1]
        .as_f64()
        .context("Missing captured field height")?
        * 60.;
    ensure!(
        width.is_finite() && height.is_finite() && width > 0. && height > 0.,
        "Invalid captured sky field."
    );
    ensure!(
        native_scale.is_finite() && native_scale > 0. && max_side >= 2,
        "Invalid sky grid scale."
    );
    let scale = native_scale.max(width.max(height) / max_side as f64);
    let (w, h) = (
        (width / scale).ceil().clamp(2., max_side as f64) as u32,
        (height / scale).ceil().clamp(2., max_side as f64) as u32,
    );
    let angle = view["rotation_deg"].as_f64().unwrap_or(0.).to_radians();
    let (s, c) = angle.sin_cos();
    let header = json!({
        "NAXIS":2,"NAXIS1":w,"NAXIS2":h,"CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN",
        "CUNIT1":"deg","CUNIT2":"deg","RADESYS":"ICRS",
        "CRVAL1":view["center_icrs_deg"][0],"CRVAL2":view["center_icrs_deg"][1],
        "CRPIX1":(w as f64+1.)/2.,"CRPIX2":(h as f64+1.)/2.,
        "CD1_1":-scale/3600.*c,"CD1_2":scale/3600.*s,"CD2_1":scale/3600.*s,"CD2_2":scale/3600.*c
    });
    SkyWcs::new(&header, w, h)?;
    Ok(
        json!({"width":w,"height":h,"wcs":header,"pixel_scale_arcsec":scale,"region_sky":view["corners_icrs_deg"]}),
    )
}

pub fn union(inputs: &[Value], max_side: u32) -> Result<Value> {
    ensure!(
        !inputs.is_empty() && max_side >= 2,
        "No science footprints to align."
    );
    let mut points = Vec::new();
    let mut direction = [0.; 3];
    let mut scale = f64::INFINITY;
    for input in inputs {
        let (width, height) = dimensions(input)?;
        let wcs = SkyWcs::new(&input["wcs"], width, height)?;
        let center = wcs
            .pixel_to_sky([(width as f64 - 1.) / 2., (height as f64 - 1.) / 2.])
            .context("Invalid source sky centre")?;
        let (ra, dec) = (center[0].to_radians(), center[1].to_radians());
        direction[0] += dec.cos() * ra.cos();
        direction[1] += dec.cos() * ra.sin();
        direction[2] += dec.sin();
        scale = scale.min(
            input["pixel_scale_arcsec"]
                .as_f64()
                .context("Missing science sampling")?,
        );
        for point in [
            [-0.5, -0.5],
            [width as f64 - 0.5, -0.5],
            [width as f64 - 0.5, height as f64 - 0.5],
            [-0.5, height as f64 - 0.5],
        ] {
            points.push(
                wcs.pixel_to_sky(point)
                    .context("Source footprint extends outside its projection")?,
            );
        }
    }
    ensure!(
        scale.is_finite()
            && scale > 0.
            && direction.iter().map(|value| value * value).sum::<f64>() > 1e-20,
        "Selected images have no local sky centre."
    );
    let center = [
        direction[1]
            .atan2(direction[0])
            .to_degrees()
            .rem_euclid(360.),
        direction[2]
            .atan2(direction[0].hypot(direction[1]))
            .to_degrees(),
    ];
    loop {
        let mut header = json!({
            "NAXIS":2,"NAXIS1":2,"NAXIS2":2,"CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN",
            "RADESYS":"ICRS","CUNIT1":"deg","CUNIT2":"deg","CRVAL1":center[0],"CRVAL2":center[1],
            "CRPIX1":1.,"CRPIX2":1.,"CDELT1":-scale/3600.,"CDELT2":scale/3600.
        });
        let wcs = SkyWcs::new(&header, 2, 2)?;
        let mut lo = [f64::INFINITY; 2];
        let mut hi = [f64::NEG_INFINITY; 2];
        for point in &points {
            let pixel = wcs
                .sky_to_pixel(*point)
                .context("Selected images span more than one TAN sky hemisphere.")?;
            for axis in 0..2 {
                lo[axis] = lo[axis].min(pixel[axis]);
                hi[axis] = hi[axis].max(pixel[axis]);
            }
        }
        let span = [
            (hi[0] - lo[0]).ceil().max(2.),
            (hi[1] - lo[1]).ceil().max(2.),
        ];
        if span[0].max(span[1]) > max_side as f64 {
            scale *= span[0].max(span[1]) / max_side as f64 * (1. + 1e-8);
            continue;
        }
        let (width, height) = (span[0] as u32, span[1] as u32);
        header["NAXIS1"] = json!(width);
        header["NAXIS2"] = json!(height);
        header["CRPIX1"] = json!(0.5 - lo[0]);
        header["CRPIX2"] = json!(0.5 - lo[1]);
        return Ok(json!({"width":width,"height":height,"wcs":header,"pixel_scale_arcsec":scale}));
    }
}

fn dimensions(meta: &Value) -> Result<(u32, u32)> {
    let dimension = |key: &str| -> Result<u32> {
        let value = meta[key].as_u64().context("Missing sky grid dimensions")?;
        ensure!(
            value > 0 && value <= u32::MAX as u64,
            "Invalid sky grid dimensions."
        );
        Ok(value as u32)
    };
    Ok((dimension("width")?, dimension("height")?))
}

struct GridMap<'a> {
    old: &'a Value,
    new: &'a Value,
    source: SkyWcs,
    target: SkyWcs,
    ratio: f64,
    translation: Option<[f64; 2]>,
}

impl<'a> GridMap<'a> {
    fn new(old: &'a Value, new: &'a Value) -> Result<Self> {
        let (ow, oh) = dimensions(old)?;
        let (nw, nh) = dimensions(new)?;
        let source = SkyWcs::new(&old["wcs"], ow, oh)?;
        let target = SkyWcs::new(&new["wcs"], nw, nh)?;
        let ratio = old["pixel_scale_arcsec"]
            .as_f64()
            .context("Missing original pixel scale")?
            / new["pixel_scale_arcsec"]
                .as_f64()
                .context("Missing target pixel scale")?;
        let mut translation = None;
        let comparable = |header: &Value| {
            let mut value = header.as_object().unwrap().clone();
            for key in ["CRPIX1", "CRPIX2", "NAXIS", "NAXIS1", "NAXIS2"] {
                value.remove(key);
            }
            value
        };
        if ratio == 1. && comparable(&source.header) == comparable(&target.header) {
            let delta = [
                target.header["CRPIX1"].as_f64().unwrap()
                    - source.header["CRPIX1"].as_f64().unwrap(),
                nh as f64
                    - oh as f64
                    - (target.header["CRPIX2"].as_f64().unwrap()
                        - source.header["CRPIX2"].as_f64().unwrap()),
            ];
            if delta
                .iter()
                .all(|value| (value - value.round()).abs() < 1e-8)
            {
                translation = Some(delta.map(f64::round));
            }
        }
        Ok(Self {
            old,
            new,
            source,
            target,
            ratio,
            translation,
        })
    }

    fn point(&self, point: [f64; 2]) -> Result<[f64; 2]> {
        if let Some(delta) = self.translation {
            return Ok([point[0] + delta[0], point[1] + delta[1]]);
        }
        let sky = self
            .source
            .pixel_to_sky([
                point[0] - 0.5,
                self.old["height"].as_f64().unwrap() - point[1] - 0.5,
            ])
            .context("Edits leave the original sky projection.")?;
        let pixel = self
            .target
            .sky_to_pixel(sky)
            .context("Edits cannot be mapped onto this sky projection.")?;
        Ok([
            pixel[0] + 0.5,
            self.new["height"].as_f64().unwrap() - pixel[1] - 0.5,
        ])
    }
}

fn pair(value: &Value) -> Result<[f64; 2]> {
    Ok([
        value[0].as_f64().context("Invalid geometry coordinate")?,
        value[1].as_f64().context("Invalid geometry coordinate")?,
    ])
}

fn numeric(value: &Value, key: &str, default: f64) -> f64 {
    value[key].as_f64().unwrap_or(default)
}

fn output(point: [f64; 2], meta: &Value, settings: &Value, inverse: bool) -> [f64; 2] {
    let center = [
        numeric(meta, "width", 0.) / 2.,
        numeric(meta, "height", 0.) / 2.,
    ];
    let (x, y) = (point[0] - center[0], point[1] - center[1]);
    let (s, c) = numeric(settings, "rotation", 0.).to_radians().sin_cos();
    let flip = if settings["flipX"].as_bool().unwrap_or(false) {
        -1.
    } else {
        1.
    };
    let result = if inverse {
        [c * flip * x - s * y, s * flip * x + c * y]
    } else {
        [flip * (c * x + s * y), -s * x + c * y]
    };
    [result[0] + center[0], result[1] + center[1]]
}

fn bounds(points: &[[f64; 2]]) -> ([f64; 2], [f64; 2]) {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for point in points {
        for axis in 0..2 {
            lo[axis] = lo[axis].min(point[axis]);
            hi[axis] = hi[axis].max(point[axis]);
        }
    }
    (lo, hi)
}

fn remap_snapshot(state: &mut Value, mapping: &GridMap<'_>) -> Result<()> {
    let (ow, oh, nw, nh) = (
        numeric(mapping.old, "width", 0.),
        numeric(mapping.old, "height", 0.),
        numeric(mapping.new, "width", 0.),
        numeric(mapping.new, "height", 0.),
    );
    if let Some(crop) = state["global"].get("crop").filter(|crop| crop.is_object()) {
        let mut crop = crop.clone();
        let full = [("x", 0.), ("y", 0.), ("w", 1.), ("h", 1.)]
            .iter()
            .all(|(key, value)| crop[*key].as_f64() == Some(*value));
        let mut settings = state["global"].clone();
        settings["rotation"] = json!(if full {
            numeric(&settings, "rotation", 0.)
        } else {
            numeric(&crop, "rotation", numeric(&settings, "rotation", 0.))
        });
        if full {
            crop.as_object_mut().unwrap().remove("rotation");
        }
        let (x, y, w, h) = (
            numeric(&crop, "x", 0.) * ow,
            numeric(&crop, "y", 0.) * oh,
            numeric(&crop, "w", 1.) * ow,
            numeric(&crop, "h", 1.) * oh,
        );
        let (lo, hi) = if let Some(delta) = mapping.translation {
            let old_center = [ow / 2., oh / 2.];
            let moved = output(
                [old_center[0] + delta[0], old_center[1] + delta[1]],
                mapping.new,
                &settings,
                true,
            );
            let lo = [x + moved[0] - old_center[0], y + moved[1] - old_center[1]];
            (lo, [lo[0] + w, lo[1] + h])
        } else {
            let points = [[x, y], [x + w, y], [x, y + h], [x + w, y + h]]
                .into_iter()
                .map(|point| {
                    Ok(output(
                        mapping.point(output(point, mapping.old, &settings, false))?,
                        mapping.new,
                        &settings,
                        true,
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            bounds(&points)
        };
        crop["x"] = json!(lo[0] / nw);
        crop["y"] = json!(lo[1] / nh);
        crop["w"] = json!((hi[0] - lo[0]) / nw);
        crop["h"] = json!((hi[1] - lo[1]) / nh);
        state["global"]["crop"] = crop;
    }
    if let Some(layers) = state["layers"].as_array_mut() {
        for layer in layers {
            if let Some(transform) = layer.get_mut("transform").filter(|value| value.is_object()) {
                let offset = [
                    numeric(transform, "x", 0.) * ow,
                    numeric(transform, "y", 0.) * oh,
                ];
                let (s, c) = numeric(transform, "rotation", 0.).to_radians().sin_cos();
                let scale = numeric(transform, "scale", 1.);
                let delta = if let Some(translation) = mapping.translation {
                    let displacement = [
                        ow / 2. + translation[0] - nw / 2.,
                        oh / 2. + translation[1] - nh / 2.,
                    ];
                    [
                        offset[0] + displacement[0]
                            - scale * (c * displacement[0] - s * displacement[1]),
                        offset[1] + displacement[1]
                            - scale * (s * displacement[0] + c * displacement[1]),
                    ]
                } else {
                    let reverse = GridMap::new(mapping.new, mapping.old)?;
                    let inverse = reverse.point([nw / 2., nh / 2.])?;
                    let (x, y) = (inverse[0] - ow / 2., inverse[1] - oh / 2.);
                    let mapped = mapping.point([
                        scale * (c * x - s * y) + ow / 2. + offset[0],
                        scale * (s * x + c * y) + oh / 2. + offset[1],
                    ])?;
                    [mapped[0] - nw / 2., mapped[1] - nh / 2.]
                };
                transform["x"] = json!(delta[0] / nw);
                transform["y"] = json!(delta[1] / nh);
            }
            if let Some(strokes) = layer["brush"]
                .get_mut("strokes")
                .and_then(Value::as_array_mut)
            {
                for stroke in strokes {
                    if let Some(points) = stroke.get_mut("points").and_then(Value::as_array_mut) {
                        for point in points {
                            *point = json!(mapping.point(pair(point)?)?);
                        }
                    }
                    if let Some(size) = stroke["size"].as_f64() {
                        stroke["size"] = json!(size * mapping.ratio);
                    }
                }
            }
            if let Some(feather) = layer["edge"]["feather"].as_f64() {
                layer["edge"]["feather"] = json!(feather * mapping.ratio);
            }
        }
    }
    Ok(())
}

pub fn remap_state(state: &Value, old: &Value, new: &Value) -> Result<Value> {
    if state.is_null() || same(old, new) {
        return Ok(state.clone());
    }
    let mapping = GridMap::new(old, new)?;
    let mut result = state.clone();
    crate::projects::remove_star_state(&mut result);
    remap_snapshot(&mut result, &mapping)?;
    if let Some(checkpoints) = result["checkpoints"].as_array_mut() {
        for checkpoint in checkpoints {
            if let Some(state) = checkpoint.get_mut("state") {
                remap_snapshot(state, &mapping)?;
            }
        }
    }
    Ok(result)
}

pub fn remap_markers(markers: &Value, old: &Value, new: &Value) -> Result<Value> {
    if markers.is_null() || same(old, new) {
        return Ok(markers.clone());
    }
    let mapping = GridMap::new(old, new)?;
    let mut result = markers.clone();
    for marker in result.as_array_mut().context("Invalid markers")? {
        if marker["kind"] == "rect" {
            let (x1, y1, x2, y2) = (
                numeric(marker, "x1", 0.),
                numeric(marker, "y1", 0.),
                numeric(marker, "x2", 0.),
                numeric(marker, "y2", 0.),
            );
            let points = [[x1, y1], [x1, y2], [x2, y1], [x2, y2]]
                .into_iter()
                .map(|point| mapping.point(point))
                .collect::<Result<Vec<_>>>()?;
            let (lo, hi) = bounds(&points);
            for (key, value) in ["x1", "y1", "x2", "y2"]
                .into_iter()
                .zip([lo[0], lo[1], hi[0], hi[1]])
            {
                marker[key] = json!(value);
            }
        } else {
            let point = mapping.point([numeric(marker, "x", 0.), numeric(marker, "y", 0.)])?;
            marker["x"] = json!(point[0]);
            marker["y"] = json!(point[1]);
            if let Some(radius) = marker["radius"].as_f64() {
                marker["radius"] = json!(radius * mapping.ratio);
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: &Value, expected: &Value, path: &str) {
        match (actual, expected) {
            (Value::Number(a), Value::Number(b)) => {
                let (a, b) = (a.as_f64().unwrap(), b.as_f64().unwrap());
                assert!((a - b).abs() < 1e-7 * b.abs().max(1.), "{path}: {a} != {b}");
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len(), "{path}");
                for (index, (a, b)) in a.iter().zip(b).enumerate() {
                    close(a, b, &format!("{path}[{index}]"));
                }
            }
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.len(), b.len(), "{path}");
                for (key, value) in b {
                    close(&a[key], value, &format!("{path}.{key}"));
                }
            }
            _ => assert_eq!(actual, expected, "{path}"),
        }
    }

    #[test]
    fn sky_remapping_preserves_anchored_crops_brushes_transforms_and_markers() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/fixtures/grid.json")).unwrap();
        for case in cases.as_array().unwrap() {
            close(
                &remap_state(&case["state"], &case["old"], &case["new"]).unwrap(),
                &case["expected_state"],
                "state",
            );
            close(
                &remap_markers(&case["markers"], &case["old"], &case["new"]).unwrap(),
                &case["expected_markers"],
                "markers",
            );
            assert_eq!(
                remap_state(&case["state"], &case["old"], &case["old"]).unwrap(),
                case["state"]
            );
        }
    }

    #[test]
    fn union_contains_rotated_sources_across_ra_wrap_with_bounded_sampling() {
        let cases: Value =
            serde_json::from_str(include_str!("../tests/fixtures/grid.json")).unwrap();
        let inputs = vec![cases[1]["old"].clone(), cases[1]["new"].clone()];
        let result = union(&inputs, 128).unwrap();
        assert!(
            result["width"].as_u64().unwrap() <= 128 && result["height"].as_u64().unwrap() <= 128
        );
        let target = SkyWcs::new(
            &result["wcs"],
            result["width"].as_u64().unwrap() as u32,
            result["height"].as_u64().unwrap() as u32,
        )
        .unwrap();
        for input in inputs {
            let (width, height) = dimensions(&input).unwrap();
            let source = SkyWcs::new(&input["wcs"], width, height).unwrap();
            for point in [
                [-0.5, -0.5],
                [width as f64 - 0.5, -0.5],
                [width as f64 - 0.5, height as f64 - 0.5],
                [-0.5, height as f64 - 0.5],
            ] {
                let point = target
                    .sky_to_pixel(source.pixel_to_sky(point).unwrap())
                    .unwrap();
                assert!(point[0] >= -0.500001 && point[1] >= -0.500001);
                assert!(
                    point[0] <= result["width"].as_f64().unwrap() - 0.5 + 1e-6
                        && point[1] <= result["height"].as_f64().unwrap() - 0.5 + 1e-6
                );
            }
        }
    }
}
