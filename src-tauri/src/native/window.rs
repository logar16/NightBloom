//! Bounded crop-local source preparation. Storage origins never redefine geometry.
use super::*;
use crate::geometry::{View, canvas_extent, rounded_dimension};
use serde::{Deserialize, Serialize};

const PREPARATION_VERSION: u32 = 4;
const STRIP_ROWS: u32 = 64;
const OUTPUT_TILE_SIDE: u32 = 512;
// Global topology is one byte per pixel, not a retained full f32 science plane.
// Bound this separately; huge Native is deliberately not an unlimited-size claim.
const MAX_GLOBAL_PIXELS: usize = MAX_SOURCE_PIXELS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    pub fn pixels(self) -> Result<usize> {
        count(self.width, self.height)
    }

    fn around(bounds: [f64; 4], margin: f64, full: Region) -> Result<Self> {
        ensure!(
            bounds.iter().all(|v| v.is_finite()) && margin.is_finite(),
            "Invalid native window geometry"
        );
        let x = (bounds[0] - margin)
            .floor()
            .clamp(0., full.width as f64 - 1.) as u32;
        let y = (bounds[1] - margin)
            .floor()
            .clamp(0., full.height as f64 - 1.) as u32;
        let right = (bounds[2] + margin)
            .ceil()
            .clamp(x as f64 + 1., full.width as f64) as u32;
        let bottom = (bounds[3] + margin)
            .ceil()
            .clamp(y as f64 + 1., full.height as f64) as u32;
        Ok(Self {
            x,
            y,
            width: right - x,
            height: bottom - y,
        })
    }
}

#[derive(Debug, Serialize)]
pub(super) struct Windows {
    pub full: Region,
    pub source: Region,
    pub render: Region,
    output_pixels: usize,
    preview_pixels: usize,
    pub tiles: Vec<RenderTile>,
    edge_radius: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(crate) struct RenderTile {
    pub source: Region,
    pub render: Region,
    pub output: Region,
}

impl Windows {
    pub fn validate(&self, layers: usize) -> Result<()> {
        validate_work_budget(self.output_pixels, 0)?;
        if self.tiles.is_empty() {
            validate_work_budget(self.render.pixels()?, 0)?;
            validate_work_budget(self.source.pixels()?, layers)?;
        } else {
            for tile in &self.tiles {
                validate_work_budget(tile.render.pixels()?, 0)?;
                validate_work_budget(tile.source.pixels()?, layers)?;
            }
        }
        ensure!(
            self.full.pixels()? <= MAX_GLOBAL_PIXELS,
            "Native full-grid coverage exceeds bounded global topology processing limit"
        );
        ensure!(
            self.preview_pixels <= MAX_CANVAS_PIXELS,
            "Built preview exceeds bounded native normalization processing limit"
        );
        // Reprojection strips use the same individual work-plane limit.
        ensure!(
            count(self.full.width, self.full.height.min(STRIP_ROWS))? <= MAX_CANVAS_PIXELS,
            "Native normalization strip exceeds bounded preparation limit"
        );
        let edge_width = self
            .source
            .width
            .saturating_add(self.edge_radius.saturating_mul(2))
            .min(self.full.width);
        let edge_height = STRIP_ROWS
            .saturating_add(self.edge_radius.saturating_mul(2))
            .min(self.full.height);
        ensure!(
            count(edge_width, edge_height)? <= MAX_CANVAS_PIXELS,
            "Native edge-processing strip exceeds bounded preparation limit"
        );
        Ok(())
    }
}

fn bounds(points: impl Iterator<Item = (f64, f64)>) -> [f64; 4] {
    points.fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |b, (x, y)| [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)],
    )
}

fn corners(region: Region) -> [(f64, f64); 4] {
    let x = region.x as f64 + 0.5;
    let y = region.y as f64 + 0.5;
    let right = (region.x + region.width) as f64 - 0.5;
    let bottom = (region.y + region.height) as f64 - 0.5;
    [(x, y), (right, y), (x, bottom), (right, bottom)]
}

/// Plan both actual storage and actual accumulation allocations, including
/// interpolation, finite Gaussian support and coverage feather neighbourhoods.
pub(super) fn preparation_windows(meta: &Value, state: &Value, factor: f64) -> Result<Windows> {
    let (bw, bh) = (dimension(meta, "width")?, dimension(meta, "height")?);
    let full = Region {
        x: 0,
        y: 0,
        width: rounded_dimension(bw as f64 * factor)?,
        height: rounded_dimension(bh as f64 * factor)?,
    };
    let (pw, ph) = crate::geometry::view_dimensions(meta, state)?;
    let output_pixels = count(
        rounded_dimension(pw as f64 * factor)?,
        rounded_dimension(ph as f64 * factor)?,
    )?;
    validate_work_budget(output_pixels, 0)?;
    let preview_pixels = count(bw, bh)?;
    let (needed, _) = required(meta, state)?;
    // Preserve the established mosaic calibration/feather path in this bounded
    // deliverable. Mosaics retain explicit full-canvas admission, not fake cropping.
    if needed
        .iter()
        .any(|l| l["tiles"].as_array().is_some_and(|t| !t.is_empty()))
    {
        return Ok(Windows {
            full,
            source: full,
            render: full,
            output_pixels,
            preview_pixels,
            tiles: Vec::new(),
            edge_radius: 0,
        });
    }
    let mut scaled = meta.clone();
    let extent = canvas_extent(meta)?.map(|v| v * factor);
    scaled["_canvas_extent"] = json!(extent);
    let view = View::new(&scaled, state)?;
    let f = view.frame;
    let output_bounds = bounds(
        [
            (f.x, f.y),
            (f.x + f.w, f.y),
            (f.x, f.y + f.h),
            (f.x + f.w, f.y + f.h),
        ]
        .into_iter()
        .map(|(x, y)| {
            let (x, y) = view.image_point(x, y);
            (x * extent[0], y * extent[1])
        }),
    );
    let layers = state["layers"].as_array().context("Missing edit layers")?;
    let mut detail_halo = 0_f64;
    let mut edge_halo = factor;
    for layer in layers
        .iter()
        .filter(|l| l["visible"].as_bool() != Some(false))
    {
        if number(&layer["detail"], "amount", 0.) > 0. {
            let radius = number(&layer["detail"], "radius", 8.) as f32;
            let sigma = radius.clamp(1., 64.) * factor as f32;
            detail_halo = detail_halo.max((4. * sigma + 0.5).floor() as f64);
        }
        let feather = number(&layer["edge"], "feather", 0.);
        let irregularity = number(&layer["edge"], "irregularity", 0.).clamp(0., 1.);
        // EDT is half-quantized by the renderer. Extra support covers rounding
        // near the maximum effective feather, not just the nominal radius.
        edge_halo = edge_halo.max(feather * factor * (1. + 0.8 * irregularity));
    }
    ensure!(
        detail_halo <= 65536.,
        "detail Gaussian radius exceeds supported sampling scale"
    );
    let render = Region::around(output_bounds, detail_halo + 3., full)?;
    let (w, h) = (extent[0] as f32, extent[1] as f32);
    let mut source_bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for layer in layers
        .iter()
        .filter(|l| l["visible"].as_bool() != Some(false))
    {
        let transform = crate::render::Transform::parse(&layer["transform"])?;
        for (x, y) in corners(render) {
            let (qx, qy) = transform.point(x as f32 / w, y as f32 / h, w, h);
            let (x, y) = ((qx * w) as f64, (qy * h) as f64);
            source_bounds[0] = source_bounds[0].min(x);
            source_bounds[1] = source_bounds[1].min(y);
            source_bounds[2] = source_bounds[2].max(x);
            source_bounds[3] = source_bounds[3].max(y);
        }
    }
    let source = Region::around(source_bounds, edge_halo * 1.001 + 4., full)?;
    let output_width = rounded_dimension(pw as f64 * factor)?;
    let output_height = rounded_dimension(ph as f64 * factor)?;
    let mut tiles = Vec::new();
    for y in (0..output_height).step_by(OUTPUT_TILE_SIDE as usize) {
        for x in (0..output_width).step_by(OUTPUT_TILE_SIDE as usize) {
            let output = Region {
                x,
                y,
                width: OUTPUT_TILE_SIDE.min(output_width - x),
                height: OUTPUT_TILE_SIDE.min(output_height - y),
            };
            let bounds = bounds(
                [
                    (output.x as f64, output.y as f64),
                    ((output.x + output.width) as f64, output.y as f64),
                    (output.x as f64, (output.y + output.height) as f64),
                    (
                        (output.x + output.width) as f64,
                        (output.y + output.height) as f64,
                    ),
                ]
                .into_iter()
                .map(|(x, y)| {
                    let (x, y) = view.image_point(
                        f.x + x / output_width as f64 * f.w,
                        f.y + y / output_height as f64 * f.h,
                    );
                    (x * extent[0], y * extent[1])
                }),
            );
            let accumulation = Region::around(bounds, detail_halo + 3., full)?;
            let mut required_bounds = [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ];
            for layer in layers
                .iter()
                .filter(|l| l["visible"].as_bool() != Some(false))
            {
                let transform = crate::render::Transform::parse(&layer["transform"])?;
                for (x, y) in corners(accumulation) {
                    let (qx, qy) = transform.point(x as f32 / w, y as f32 / h, w, h);
                    let (x, y) = ((qx * w) as f64, (qy * h) as f64);
                    required_bounds[0] = required_bounds[0].min(x);
                    required_bounds[1] = required_bounds[1].min(y);
                    required_bounds[2] = required_bounds[2].max(x);
                    required_bounds[3] = required_bounds[3].max(y);
                }
            }
            // Edges are prepared already. Loading needs only bilinear support,
            // not a second feather halo per retained science plane.
            let requested = Region::around(required_bounds, 4., full)?;
            let left = requested.x.max(source.x);
            let top = requested.y.max(source.y);
            let right = (requested.x + requested.width)
                .min(source.x + source.width)
                .max(left + 1);
            let bottom = (requested.y + requested.height)
                .min(source.y + source.height)
                .max(top + 1);
            tiles.push(RenderTile {
                source: Region {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                },
                render: accumulation,
                output,
            });
        }
    }
    Ok(Windows {
        full,
        source,
        render,
        output_pixels,
        preview_pixels,
        tiles,
        edge_radius: ((edge_halo.min((1024. * factor as f32).min(65504.) as f64) * 1.001 + 4.)
            .ceil() as u32)
            .min(full.width.max(full.height)),
    })
}

pub(crate) fn render_window(meta: &Value) -> Result<Region> {
    let region = match meta.get("_render_window") {
        Some(value) => {
            let region: Region =
                serde_json::from_value(value.clone()).context("Invalid native render window")?;
            validate_work_budget(region.pixels()?, 0)?;
            region
        }
        None => Region {
            x: 0,
            y: 0,
            width: dimension(meta, "width")?,
            height: dimension(meta, "height")?,
        },
    };
    Ok(region)
}

pub(crate) fn window_origin(meta: &Value) -> Result<[f32; 2]> {
    let Some(origin) = meta.get("_storage_origin") else {
        return Ok([0., 0.]);
    };
    let origin: [u32; 2] =
        serde_json::from_value(origin.clone()).context("Invalid native storage origin")?;
    Ok(origin.map(|v| v as f32))
}

/// Exact area-preview reduction over full physical reprojection, with bounded
/// strips. The per-preview-pixel addition order remains top-down yy then xx,
/// including fractional overlap and the original 98% validity threshold.
fn streamed_preview(
    source: &Raster,
    wcs: &Celestial,
    target: &Celestial,
    windows: &Windows,
    factor: f64,
    preview_dimensions: (u32, u32),
    cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<f32>, Vec<bool>)> {
    let (bw, bh) = preview_dimensions;
    let full = windows.full;
    let mut sums = vec![0_f64; count(bw, bh)?];
    let mut areas = vec![0_f64; sums.len()];
    let mut covered = vec![false; full.pixels()?];
    for y in (0..full.height).step_by(STRIP_ROWS as usize) {
        check(cancelled)?;
        let strip = Region {
            x: 0,
            y,
            width: full.width,
            height: STRIP_ROWS.min(full.height - y),
        };
        let raw = reproject_region(source, wcs, target, full.height, strip, cancelled)?;
        for yy in y..y + strip.height {
            check(cancelled)?;
            // Include adjacent candidates at exact mathematical boundaries:
            // multiplication/division can round on opposite sides by one ULP.
            let by0 = ((yy as f64 / factor).floor().max(0.) as u32).saturating_sub(1);
            let by1 = (((yy + 1) as f64 / factor).ceil() as u32)
                .saturating_add(1)
                .min(bh);
            for xx in 0..full.width {
                let value = raw[((yy - y) * full.width + xx) as usize];
                let finite = value.is_finite();
                covered[(yy * full.width + xx) as usize] = finite;
                if !finite {
                    continue;
                }
                let bx0 = ((xx as f64 / factor).floor().max(0.) as u32).saturating_sub(1);
                let bx1 = (((xx + 1) as f64 / factor).ceil() as u32)
                    .saturating_add(1)
                    .min(bw);
                for by in by0..by1 {
                    let (y0, y1) = (by as f64 * factor, (by + 1) as f64 * factor);
                    if y1 > full.height as f64 + 1e-9 {
                        continue;
                    }
                    let y_area = ((yy + 1) as f64).min(y1).sub_max(y0.max(yy as f64));
                    if y_area == 0. {
                        continue;
                    }
                    for bx in bx0..bx1 {
                        let (x0, x1) = (bx as f64 * factor, (bx + 1) as f64 * factor);
                        if x1 > full.width as f64 + 1e-9 {
                            continue;
                        }
                        let area = ((xx + 1) as f64).min(x1).sub_max(x0.max(xx as f64)) * y_area;
                        let i = (by * bw + bx) as usize;
                        sums[i] += value as f64 * area;
                        areas[i] += area;
                    }
                }
            }
        }
    }
    let small = sums
        .into_iter()
        .zip(areas)
        .map(|(sum, area)| {
            if area > factor * factor * 0.98 {
                (sum / area) as f32
            } else {
                f32::NAN
            }
        })
        .collect();
    Ok((small, covered))
}

fn extract<T: Copy>(full: &[T], width: u32, region: Region) -> Vec<T> {
    let mut local = Vec::with_capacity(region.width as usize * region.height as usize);
    for y in region.y..region.y + region.height {
        let i = (y * width + region.x) as usize;
        local.extend_from_slice(&full[i..i + region.width as usize]);
    }
    local
}

fn write_edge(
    destination: &Path,
    name: &str,
    mut covered: Vec<bool>,
    windows: &Windows,
    scale: f64,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    // Hole size/connectivity must be classified on the FULL science footprint.
    // A long thin globally small hole cannot be mistaken for a crop boundary.
    crate::planes::fill_small_holes(
        &mut covered,
        windows.full.width as usize,
        windows.full.height as usize,
        4096. * (scale as f32 as f64).powi(2),
        cancelled,
    )?;
    let cap = (1024. * scale as f32).min(65504.);
    let mut file = BufWriter::new(File::create(destination.join(name))?);
    for y in
        (windows.source.y..windows.source.y + windows.source.height).step_by(STRIP_ROWS as usize)
    {
        check(cancelled)?;
        let strip = Region {
            x: windows.source.x,
            y,
            width: windows.source.width,
            height: STRIP_ROWS.min(windows.source.y + windows.source.height - y),
        };
        let expanded = Region::around(
            [
                strip.x as f64,
                strip.y as f64,
                (strip.x + strip.width) as f64,
                (strip.y + strip.height) as f64,
            ],
            windows.edge_radius as f64,
            windows.full,
        )?;
        ensure!(
            expanded.pixels()? <= MAX_CANVAS_PIXELS,
            "Native edge strip exceeds preparation budget"
        );
        let local = extract(&covered, windows.full.width, expanded);
        // Full-grid hole classification, no artificial padding, and enough
        // distance support to preserve all unsaturated feather/coverage alpha.
        let distance = distances(
            &local,
            expanded.width as usize,
            expanded.height as usize,
            false,
            cancelled,
        )?;
        for yy in strip.y..strip.y + strip.height {
            for xx in strip.x..strip.x + strip.width {
                let d = distance[((yy - expanded.y) * expanded.width + xx - expanded.x) as usize];
                file.write_all(&f16::from_f32(d.min(cap)).to_bits().to_le_bytes())?;
            }
        }
    }
    file.flush()?;
    Ok(())
}

fn write_luminance(
    destination: &Path,
    layers: &[Value],
    region: Region,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let mut inputs = layers
        .iter()
        .map(|layer| -> Result<_> {
            Ok((
                BufReader::new(File::open(local_file(
                    destination,
                    layer["file"].as_str().context("Missing native plane")?,
                )?)?),
                BufReader::new(File::open(local_file(
                    destination,
                    layer["coverage_file"]
                        .as_str()
                        .context("Missing native coverage")?,
                )?)?),
            ))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut output = BufWriter::new(File::create(destination.join("luminance.f32"))?);
    for y in (0..region.height).step_by(STRIP_ROWS as usize) {
        check(cancelled)?;
        let n = count(region.width, STRIP_ROWS.min(region.height - y))?;
        let mut sums = vec![0_f32; n];
        let mut counts = vec![0_u32; n];
        let mut bytes = vec![0_u8; n * 4];
        let mut covered = vec![0_u8; n];
        // The source order and f32 additions match the original full-canvas
        // luminance exactly; only the retained strip is smaller.
        for (plane, coverage) in &mut inputs {
            check(cancelled)?;
            plane.read_exact(&mut bytes)?;
            coverage.read_exact(&mut covered)?;
            for (i, pixel) in bytes.as_chunks::<4>().0.iter().enumerate() {
                if i % 16384 == 0 {
                    check(cancelled)?;
                }
                if covered[i] > 0 {
                    sums[i] += f32::from_le_bytes(*pixel);
                    counts[i] += 1;
                }
            }
        }
        for (sum, count) in sums.into_iter().zip(counts) {
            output.write_all(&(if count > 0 { sum / count as f32 } else { 0. }).to_le_bytes())?;
        }
    }
    output.flush()?;
    Ok(())
}

pub(super) fn prepare_window_project(
    session: &Path,
    state: &Value,
    info: &ExportPlan,
    windows: &Windows,
    destination: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Value> {
    let folder = session.join("project");
    let mut meta = read_json(&folder.join("meta.json"))?;
    let (needed, _) = required(&meta, state)?;
    windows.validate(needed.len())?;
    if needed
        .iter()
        .any(|l| l["tiles"].as_array().is_some_and(|t| !t.is_empty()))
    {
        return prepare_project(session, state, info, destination, cancelled);
    }
    let (bw, bh) = (dimension(&meta, "width")?, dimension(&meta, "height")?);
    let grid = Celestial::new(&meta["wcs"], bw, bh)?;
    let full = windows.full;
    let region = windows.source;
    let mut target = grid.scaled(info.factor, full.width, full.height)?;
    let shift = full.height as f64 - bh as f64 * info.factor;
    target.header["CRPIX2"] = json!(number(&target.header, "CRPIX2", 0.) + shift);
    target = Celestial::new(&target.header, full.width, full.height)?;
    let mut lum_coverage = vec![false; full.pixels()?];
    let mut layers = Vec::new();
    for (index, mut layer) in needed.into_iter().enumerate() {
        check(cancelled)?;
        let source = Science::open(&source_path(session, &layer)?)?;
        let block = (target.scale / source.wcs.scale + 1e-8).floor().max(1.) as u32;
        let (pixels, wcs) = source.prepared(block, cancelled)?;
        let (small, covered) = streamed_preview(
            &pixels,
            &wcs,
            &target,
            windows,
            info.factor,
            (bw, bh),
            cancelled,
        )?;
        let survey_tone = survey_normalization(&layer)?;
        let (a, b) = if let Some(tone) = survey_tone {
            tone
        } else {
            let preview = read_half(&folder, &layer["file"], bw, bh, cancelled)?;
            let mut valid = vec![true; count(bw, bh)?];
            if let Some(name) = layer["coverage_file"].as_str() {
                let coverage = fs::read(local_file(&folder, name)?)?;
                ensure!(
                    coverage.len() == valid.len(),
                    "Invalid built coverage for tone fit"
                );
                for (v, c) in valid.iter_mut().zip(coverage) {
                    *v &= c > 0;
                }
            }
            fit_tone(&small, &preview, &valid)?
        };
        drop(small);
        let file = format!("native-{index}.f32");
        let coverage_file = format!("native-{index}.coverage.u8");
        let edge_file = format!("native-{index}.edge.f16");
        let mut out = BufWriter::new(File::create(destination.join(&file))?);
        let mut coverage_out = BufWriter::new(File::create(destination.join(&coverage_file))?);
        for y in (region.y..region.y + region.height).step_by(STRIP_ROWS as usize) {
            check(cancelled)?;
            let strip = Region {
                x: region.x,
                y,
                width: region.width,
                height: STRIP_ROWS.min(region.y + region.height - y),
            };
            let raw = reproject_region(&pixels, &wcs, &target, full.height, strip, cancelled)?;
            for (i, v) in raw.into_iter().enumerate() {
                if i % 16384 == 0 {
                    check(cancelled)?;
                }
                let finite = v.is_finite();
                let value = if finite {
                    ((v - a as f32) / b as f32).clamp(-65504., 65504.)
                } else {
                    0.
                };
                out.write_all(&value.to_le_bytes())?;
                coverage_out.write_all(&[u8::from(finite)])?;
            }
        }
        drop(pixels);
        out.flush()?;
        coverage_out.flush()?;
        for (i, (all, source)) in lum_coverage.iter_mut().zip(&covered).enumerate() {
            if i % 16384 == 0 {
                check(cancelled)?;
            }
            *all |= *source;
        }
        write_edge(
            destination,
            &edge_file,
            covered,
            windows,
            info.factor,
            cancelled,
        )?;
        layer["file"] = json!(file);
        layer["coverage_file"] = json!(coverage_file);
        layer["edge_file"] = json!(edge_file);
        layer
            .as_object_mut()
            .context("Invalid layer")?
            .remove("starless");
        layer["native_normalization"] = json!({"offset":a,"scale":b,"method":if survey_tone.is_some() {"persisted survey background/scale"} else {"preview robust raw fit"}});
        layers.push(layer);
    }
    write_luminance(destination, &layers, region, cancelled)?;
    write_edge(
        destination,
        "luminance.edge.f16",
        lum_coverage,
        windows,
        info.factor,
        cancelled,
    )?;
    let extent = canvas_extent(&meta)?.map(|v| v * info.factor);
    meta["width"] = json!(region.width);
    meta["height"] = json!(region.height);
    meta["_canvas_extent"] = json!(extent);
    meta["_storage_origin"] = json!([region.x, region.y]);
    meta["_render_window"] = json!(windows.render);
    meta["_native_tiles"] = json!(windows.tiles);
    meta["_full_canvas_size"] = json!([full.width, full.height]);
    // WCS describes local storage; canvas extent and window origins describe
    // the unchanged global editor geometry independently.
    target.header["CRPIX1"] = json!(number(&target.header, "CRPIX1", 0.) - region.x as f64);
    target.header["CRPIX2"] = json!(
        number(&target.header, "CRPIX2", 0.) - (full.height - region.y - region.height) as f64
    );
    meta["wcs"] = target.header;
    meta["pixel_scale_arcsec"] = json!(target.scale * 3600.);
    meta["_detail_scale"] = json!(info.factor);
    meta["layers"] = json!(layers);
    meta["luminance"] = json!({"file":"luminance.f32","edge_file":"luminance.edge.f16"});
    meta.as_object_mut()
        .context("Invalid project metadata")?
        .remove("stars");
    meta["_native_export"] = json!({"source_backed":true,"factor":info.factor,
        "preparation_version":PREPARATION_VERSION,"windows":windows,
        "estimate_model":"Python empirical non-star source/work model; not Rust timing"});
    Ok(meta)
}

/// Disposable preparation stays in the owning library, never AppData.
pub(super) fn export_cache(session: &Path) -> Result<PathBuf> {
    let cache = session
        .ancestors()
        .find(|p| p.file_name().is_some_and(|n| n == "sessions"))
        .and_then(Path::parent)
        .map(|root| {
            root.join("cache/export")
                .join(session.file_name().unwrap_or_default())
        })
        .unwrap_or_else(|| session.join(".scratch/export"));
    fs::create_dir_all(&cache).context("Cannot create export cache in the library")?;
    Ok(cache)
}

pub(super) fn identity(
    session: &Path,
    meta: &Value,
    state: &Value,
    info: &ExportPlan,
    windows: &Windows,
    memo: &mut super::fingerprint::Fingerprints,
    cancelled: &dyn Fn() -> bool,
) -> Result<Value> {
    let (mut needed, _) = required(meta, state)?;
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for layer in &mut needed {
        let tiles: Vec<&Value> = layer["tiles"]
            .as_array()
            .filter(|t| !t.is_empty())
            .map_or_else(|| vec![&*layer], |t| t.iter().collect());
        for tile in tiles {
            let path = source_path(session, tile)?;
            if seen.insert(path.clone()) {
                files.push(content_identity(memo.fingerprint(&path, cancelled)?));
            }
        }
        for field in ["file", "coverage_file"] {
            if let Some(name) = layer[field].as_str() {
                let path = local_file(&session.join("project"), name)?;
                if seen.insert(path.clone()) {
                    files.push(content_identity(memo.fingerprint(&path, cancelled)?));
                }
            }
        }
        let object = layer.as_object_mut().context("Invalid layer metadata")?;
        object.remove("starless");
        object.remove("edge_file");
    }
    Ok(
        json!({"version":PREPARATION_VERSION,"session":session.canonicalize()?,
        "grid":meta["wcs"],"width":meta["width"],"height":meta["height"],
        "extent":canvas_extent(meta)?,"factor":info.factor,"windows":windows,
        "layers":needed,"inputs":files}),
    )
}

fn content_identity(mut fingerprint: Value) -> Value {
    // Change tokens decide whether a strong digest must be recalculated. Only
    // content/provenance identify science preparation: harmless metadata changes
    // (e.g. background NTFS compression) must not manufacture source mutations.
    if let Some(object) = fingerprint.as_object_mut() {
        object.remove("change_token");
    }
    fingerprint
}

pub(super) fn refresh_fingerprints(
    cache: &Path,
    identity: &Value,
    memo: &super::fingerprint::Fingerprints,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    check(cancelled)?;
    let mut latest = read_json(&cache.join("latest.json"))?;
    ensure!(
        latest["identity"] == *identity,
        "Native preparation changed during rendering"
    );
    let fingerprints = memo.snapshot();
    if latest["fingerprints"] != fingerprints {
        latest["fingerprints"] = fingerprints;
        crate::projects::write_json(&cache.join("latest.json"), &latest)?;
    }
    Ok(())
}

pub(super) fn cached(cache: &Path, identity: &Value) -> Result<Option<PathBuf>> {
    let latest = crate::projects::read_json(&cache.join("latest.json"), Value::Null)?;
    if latest["identity"] != *identity {
        return Ok(None);
    }
    let Some(generation) = latest["generation"].as_str() else {
        return Ok(None);
    };
    let path = local_file(cache, generation)?;
    let Some(files) = latest["files"].as_array() else {
        return Ok(None);
    };
    for file in files {
        let Some(name) = file["name"].as_str() else {
            return Ok(None);
        };
        let metadata = match fs::metadata(local_file(&path, name)?) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("Reading native cache artifact"),
        };
        if !metadata.is_file() || Some(metadata.len()) != file["bytes"].as_u64() {
            return Ok(None);
        }
    }
    Ok(Some(path))
}

pub(super) fn publish(
    cache: &Path,
    temp: tempfile::TempDir,
    identity: &Value,
    memo: &super::fingerprint::Fingerprints,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let old = crate::projects::read_json(&cache.join("latest.json"), Value::Null)?;
    let files = fs::read_dir(temp.path())?
        .map(|entry| -> Result<Value> {
            let entry = entry?;
            Ok(
                json!({"name":entry.file_name().to_str().context("Invalid native artifact name")?,
            "bytes":entry.metadata()?.len()}),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    check(cancelled)?;
    let path = temp.keep();
    let generation = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Missing native cache generation")?;
    let latest = json!({"generation":generation,"identity":identity,"files":files,
        "fingerprints":memo.snapshot()});
    if let Err(error) = crate::projects::write_json(&cache.join("latest.json"), &latest) {
        let _ = fs::remove_dir_all(&path);
        return Err(error).context("Publishing native preparation");
    }
    // Atomic latest.json switches the generation only after preparation/render
    // success. At rest there is one generation; replacing it briefly needs two.
    if let Some(previous) = old["generation"].as_str() {
        let previous = local_file(cache, previous)?;
        if previous != path && previous.exists() {
            fs::remove_dir_all(previous).context("Removing superseded native preparation")?;
        }
    }
    Ok(())
}
