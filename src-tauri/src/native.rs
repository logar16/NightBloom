//! Source-backed, non-star native export preparation. Originals are opened read-only.

use anyhow::{Context, Result, bail, ensure};
use fitsrs::{Fits, HDU};
use half::f16;
use serde_json::{Map, Value, json};
use std::collections::{HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::contract::{ExportPlan, RenderedImage};

mod building;
mod fingerprint;
mod window;
pub use building::{
    BUILD_VERSION, BuildSource, layer_key, prepare_build, preview_source, source_metadata,
};
pub(crate) use building::{pair_fit as mosaic_pair_fit, samples as mosaic_samples};
pub(crate) use window::{Region, RenderTile, render_window, window_origin};
use window::{export_cache, preparation_windows, prepare_window_project};

// Design decision: RGB and retained science allocations are crop/window-local.
// Global normalization and hole classification are streamed from actual sources,
// not approximated from the crop or enlarged preview RGB.
// User-selected provisional budgets, not a measured process peak-RSS guarantee:
// * each f32 SCI plane: 512 MiB = 134,217,728 pixels (real f187n is 123,088,803).
//   An optional same-shaped f32 WHT costs up to another 512 MiB.
// * each canvas/output RGB buffer: 16 Mi pixels = 192 MiB.
// * required source work: 64 Mi f32 pixels = 256 MiB (four 4K 4:3 layers fit).
// Native temporary planes retain f32 science precision; the aggregate budget
// counts their 4 B/pixel. Repair copies, component masks/queues, normalization/edge/detail
// scratch, luminance and a second RGB buffer are ADDITIONAL allocations.
// Source preparation is sequential. These are individual/aggregate allocation
// bounds, not an assertion that all working memory fits in their sum.
const SOURCE_PLANE_BUDGET_BYTES: usize = 512 * 1024 * 1024;
const CANVAS_RGB_BUDGET_BYTES: usize = 192 * 1024 * 1024;
const RETAINED_PLANES_BUDGET_BYTES: usize = 256 * 1024 * 1024;
const MAX_SOURCE_PIXELS: usize = SOURCE_PLANE_BUDGET_BYTES / 4;
const MAX_CANVAS_PIXELS: usize = CANVAS_RGB_BUDGET_BYTES / 12;
const MAX_LAYER_PIXELS: usize = RETAINED_PLANES_BUDGET_BYTES / 4;

fn check(cancelled: &dyn Fn() -> bool) -> Result<()> {
    ensure!(!cancelled(), "Native export cancelled.");
    Ok(())
}

fn number(value: &Value, key: &str, default: f64) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn dimension(value: &Value, key: &str) -> Result<u32> {
    let n = value
        .get(key)
        .and_then(Value::as_u64)
        .context(format!("Missing {key}"))?;
    ensure!(n > 0 && n <= u32::MAX as u64, "Invalid {key}");
    Ok(n as u32)
}

fn count(width: u32, height: u32) -> Result<usize> {
    (width as usize)
        .checked_mul(height as usize)
        .context("Image dimensions overflow")
}

fn validate_work_budget(pixels: usize, layers: usize) -> Result<()> {
    let aggregate = pixels
        .checked_mul(layers)
        .context("Native layer dimensions overflow")?;
    ensure!(
        pixels <= MAX_CANVAS_PIXELS,
        "Native render window exceeds {} MiB RGB allocation budget ({} pixels); no preview enlargement was used.",
        CANVAS_RGB_BUDGET_BYTES / (1024 * 1024),
        MAX_CANVAS_PIXELS
    );
    ensure!(
        aggregate <= MAX_LAYER_PIXELS,
        "Native required layers exceed {} MiB f32 work allocation budget ({} aggregate pixels); no preview enlargement was used.",
        RETAINED_PLANES_BUDGET_BYTES / (1024 * 1024),
        MAX_LAYER_PIXELS
    );
    Ok(())
}

fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_reader(BufReader::new(
        File::open(path).with_context(|| format!("Cannot read {}", path.display()))?,
    ))
    .context("Invalid project JSON")
}

pub(crate) fn header_json<X: fitsrs::hdu::header::Xtension>(
    h: &fitsrs::hdu::header::Header<X>,
) -> Value {
    use fitsrs::card::Value as F;
    let mut values = Map::new();
    for (key, value) in h.iter() {
        let value = match value {
            F::Integer { value, .. } => json!(value),
            F::Float { value, .. } => json!(value),
            F::String { value, .. } => json!(value),
            F::Logical { value, .. } => json!(value),
            _ => continue,
        };
        values.insert(key.into(), value);
    }
    Value::Object(values)
}

#[derive(Clone)]
struct ImageInfo {
    index: usize,
    width: u32,
    height: u32,
    offset: u64,
    header: Value,
    compressed: bool,
}

fn image_info(
    index: usize,
    offset: u64,
    header: Value,
    is_image: bool,
) -> Result<Option<ImageInfo>> {
    let compressed = header["ZIMAGE"].as_bool().unwrap_or(false);
    if !is_image && !compressed {
        return Ok(None);
    }
    let prefix = if compressed { "Z" } else { "" };
    if header[format!("{prefix}NAXIS")].as_u64() != Some(2) {
        return Ok(None);
    }
    let width = dimension(&header, &format!("{prefix}NAXIS1"))?;
    let height = dimension(&header, &format!("{prefix}NAXIS2"))?;
    count(width, height)?;
    Ok(Some(ImageInfo {
        index,
        width,
        height,
        offset,
        header,
        compressed,
    }))
}

struct Science {
    path: PathBuf,
    image: ImageInfo,
    weight: Option<ImageInfo>,
    wcs: Celestial,
}

impl Science {
    /// Match Python science(): SCI first, otherwise first 2-D image; primary+SCI merge.
    fn open(path: &Path) -> Result<Self> {
        let reader = BufReader::new(
            File::open(path)
                .with_context(|| format!("Missing native FITS source: {}", path.display()))?,
        );
        let mut fits = Fits::from_reader(reader);
        let mut primary = json!({});
        let mut fallback = None;
        let mut science = None;
        let mut weights = Vec::new();
        for (index, entry) in Iterator::by_ref(&mut fits).enumerate() {
            let entry = entry.with_context(|| format!("Invalid FITS: {}", path.display()))?;
            let (header, image) = match &entry {
                HDU::Primary(h) | HDU::XImage(h) => (header_json(h.get_header()), true),
                HDU::XBinaryTable(h) => (header_json(h.get_header()), false),
                HDU::XASCIITable(_) => continue,
            };
            if index == 0 {
                primary = header.clone();
            }
            let name = header["EXTNAME"]
                .as_str()
                .unwrap_or("")
                .trim()
                .to_ascii_uppercase();
            let info = image_info(index, entry.get_data_unit_byte_offset(), header, image)?;
            if name == "SCI" && science.is_none() {
                science = Some(info.context("SCI must be a 2-D science image")?);
            } else if name == "WHT" {
                if let Some(info) = info {
                    weights.push(info);
                }
            } else if fallback.is_none() {
                fallback = info;
            }
        }
        let image = science
            .or(fallback)
            .context("Expected a 2-D FITS science image")?;
        validate_compression(&image)?;
        let mut merged = primary
            .as_object()
            .context("Invalid primary header")?
            .clone();
        merged.extend(image.header.as_object().unwrap().clone());
        if image.compressed {
            merged.insert("NAXIS".into(), json!(2));
            merged.insert("NAXIS1".into(), json!(image.width));
            merged.insert("NAXIS2".into(), json!(image.height));
        }
        let wcs = Celestial::new(&Value::Object(merged), image.width, image.height)
            .with_context(|| format!("Unsupported native WCS: {}", path.display()))?;
        let weight = weights
            .into_iter()
            .find(|w| w.width == image.width && w.height == image.height);
        if let Some(w) = &weight {
            validate_compression(w)?;
        }
        Ok(Self {
            path: path.into(),
            image,
            weight,
            wcs,
        })
    }

    fn load(&self, cancelled: &dyn Fn() -> bool) -> Result<Raster> {
        let values = load_image(&self.path, &self.image, cancelled)?;
        Ok(Raster {
            width: self.image.width,
            height: self.image.height,
            values,
        })
    }

    fn prepared(&self, block: u32, cancelled: &dyn Fn() -> bool) -> Result<(Raster, Celestial)> {
        let mut raster = self.load(cancelled)?;
        let mut weight = self
            .weight
            .as_ref()
            .map(|h| load_image(&self.path, h, cancelled))
            .transpose()?;
        if let Some(w) = &weight {
            repair_streaks(&mut raster, w, cancelled)?;
        }
        if block > 1 {
            if let Some(w) = weight {
                weight = Some(
                    block_mean(
                        &Raster {
                            width: raster.width,
                            height: raster.height,
                            values: w,
                        },
                        block,
                        cancelled,
                    )?
                    .values,
                );
            }
            raster = block_mean(&raster, block, cancelled)?;
        }
        if let Some(weight) = weight {
            cut_fringe(&mut raster, &weight, cancelled)?;
        }
        let wcs = self
            .wcs
            .scaled(1. / block as f64, raster.width, raster.height)?;
        Ok((raster, wcs))
    }
}

fn validate_compression(image: &ImageInfo) -> Result<()> {
    if !image.compressed {
        return Ok(());
    }
    let h = &image.header;
    let algorithm = h["ZCMPTYPE"].as_str().unwrap_or("").trim();
    ensure!(
        matches!(algorithm, "RICE_1" | "RICE_ONE" | "GZIP_1" | "GZIP_2"),
        "Unsupported FITS compression {algorithm}; HCOMPRESS/PLIO are not supported"
    );
    let bitpix = h["ZBITPIX"].as_i64().context("Missing ZBITPIX")?;
    // Additional live dependency spike: even full-row quantized f32 RICE with
    // a NaN overflows in the crate's debug decompressor. Only the bit depth
    // actually verified against Astropy is admitted in this milestone.
    ensure!(
        bitpix == 32,
        "Unsupported compressed floating/other-bit-depth science (ZBITPIX={bitpix}); only verified 32-bit integer compression is supported"
    );
    let tx = number(h, "ZTILE1", image.width as f64);
    let ty = number(h, "ZTILE2", 1.);
    // Verified fitsrs defect: a 13x11 image with 5x3 partial-edge tiles produces
    // too many pixels. Reject all unverified layouts at header validation; never
    // truncate/reorder decompressor output to disguise that defect.
    ensure!(
        tx == image.width as f64 && ty == 1.,
        "Unsupported compressed FITS tile layout: only full-width single-row tiles are verified; partial-edge/multi-row tiles are rejected before decoding"
    );
    ensure!(
        h["NAXIS2"].as_u64() == Some(image.height as u64),
        "Unsupported compressed FITS table row count"
    );
    Ok(())
}

fn load_image(path: &Path, image: &ImageInfo, cancelled: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    check(cancelled)?;
    validate_compression(image)?;
    let n = count(image.width, image.height)?;
    ensure!(
        n <= MAX_SOURCE_PIXELS,
        "Native source exceeds bounded preparation limit ({n} > {MAX_SOURCE_PIXELS} pixels): {}. No preview enlargement was used.",
        path.display()
    );
    if !image.compressed {
        let mut reader = BufReader::new(File::open(path)?);
        reader.seek(SeekFrom::Start(image.offset))?;
        let bitpix = image.header["BITPIX"].as_i64().context("Missing BITPIX")?;
        ensure!(
            matches!(bitpix, 8 | 16 | 32 | 64 | -32 | -64),
            "Unsupported BITPIX {bitpix}"
        );
        let bytes = (bitpix.unsigned_abs() / 8) as usize;
        let scale = number(&image.header, "BSCALE", 1.);
        let zero = number(&image.header, "BZERO", 0.);
        let blank = image.header["BLANK"].as_i64();
        let mut row = vec![0_u8; image.width as usize * bytes];
        let mut out = Vec::with_capacity(n);
        for _ in 0..image.height {
            check(cancelled)?;
            reader
                .read_exact(&mut row)
                .context("Truncated native FITS image")?;
            for p in row.chunks_exact(bytes) {
                let integer = match bitpix {
                    8 => Some(p[0] as i64),
                    16 => Some(i16::from_be_bytes(p.try_into()?) as i64),
                    32 => Some(i32::from_be_bytes(p.try_into()?) as i64),
                    64 => Some(i64::from_be_bytes(p.try_into()?)),
                    _ => None,
                };
                let value = match (bitpix, integer) {
                    (_, Some(i)) if blank == Some(i) => f64::NAN,
                    (_, Some(i)) => i as f64,
                    (-32, _) => f32::from_be_bytes(p.try_into()?) as f64,
                    (-64, _) => f64::from_be_bytes(p.try_into()?),
                    _ => unreachable!(),
                };
                out.push((value * scale + zero) as f32);
            }
        }
        return Ok(out);
    }
    // The public crate's compressed iterators hide I/O failures and some malformed
    // inputs panic. Contain the panic and verify the complete pixel count explicitly.
    let decoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<f32>> {
        use fitsrs::hdu::data::bintable::{data::BinaryTableData, tile_compressed::pixels::Pixels};
        let mut fits = Fits::from_reader(BufReader::new(File::open(path)?));
        let hdu = fits
            .nth(image.index)
            .context("Missing compressed image HDU")??;
        let HDU::XBinaryTable(hdu) = hdu else {
            bail!("Compressed image is not a FITS binary table");
        };
        let BinaryTableData::TileCompressed(pixels) = fits.get_data(&hdu) else {
            bail!("FITS library does not support this compressed image");
        };
        fn collect<I: Iterator>(
            it: I,
            n: usize,
            cancelled: &dyn Fn() -> bool,
            convert: impl Fn(I::Item) -> f32,
        ) -> Result<Vec<f32>> {
            let mut out = Vec::with_capacity(n);
            for (i, v) in it.enumerate() {
                if i % 16384 == 0 {
                    check(cancelled)?;
                }
                ensure!(i < n, "Compressed FITS produced too many pixels");
                out.push(convert(v));
            }
            ensure!(
                out.len() == n,
                "Truncated/unsupported compressed image: decoded {} of {n} pixels",
                out.len()
            );
            Ok(out)
        }
        let scale = number(&image.header, "BSCALE", 1.);
        let zero = number(&image.header, "BZERO", 0.);
        let blank = image.header["BLANK"]
            .as_i64()
            .or_else(|| image.header["ZBLANK"].as_i64());
        match pixels {
            Pixels::I32(it) => collect(it, n, cancelled, |v| {
                if blank == Some(v as i64) {
                    f32::NAN
                } else {
                    (v as f64 * scale + zero) as f32
                }
            }),
            _ => bail!("FITS decompressor returned an unverified bit depth"),
        }
    }))
    .map_err(|_| {
        anyhow::anyhow!("FITS decompressor rejected malformed/unsupported compression")
    })??;
    // Full-width one-row tiles already have FITS raster order. No reorder/crop.
    Ok(decoded)
}

struct Celestial {
    wcs: crate::sky::SkyWcs,
    header: Value,
    scale: f64,
    fk5: bool,
}

impl Celestial {
    fn new(header: &Value, width: u32, height: u32) -> Result<Self> {
        let mut h = header
            .as_object()
            .context("Missing celestial WCS header")?
            .clone();
        ensure!(
            h.get("CTYPE1").and_then(Value::as_str) == Some("RA---TAN")
                && h.get("CTYPE2").and_then(Value::as_str) == Some("DEC--TAN"),
            "Only undistorted RA/DEC TAN science is supported (SIP/TPV/other projections require a verified implementation)"
        );
        for key in h.keys() {
            ensure!(
                !(key.starts_with("CPDIS")
                    || key.starts_with("DP1")
                    || key.starts_with("DP2")
                    || key.starts_with("D2IMDIS")
                    || key.starts_with("DET2IM")
                    || key.starts_with("A_")
                    || key.starts_with("B_")
                    || key.starts_with("AP_")
                    || key.starts_with("BP_")
                    || key.starts_with("PV1_")
                    || key.starts_with("PV2_")),
                "Unsupported WCS distortion keyword {key}"
            );
        }
        for key in ["CUNIT1", "CUNIT2"] {
            ensure!(
                h.get(key).and_then(Value::as_str).unwrap_or("deg").trim() == "deg",
                "Unsupported WCS units in {key}"
            );
        }
        let frame = h
            .get("RADESYS")
            .or_else(|| h.get("RADECSYS"))
            .and_then(Value::as_str)
            .unwrap_or(if h.contains_key("EQUINOX") {
                "FK5"
            } else {
                "ICRS"
            })
            .trim();
        ensure!(
            frame == "ICRS" || frame == "FK5",
            "Unsupported celestial frame {frame}"
        );
        let fk5 = frame == "FK5";
        ensure!(
            !fk5 || (h.get("EQUINOX").and_then(Value::as_f64).unwrap_or(2000.) - 2000.).abs()
                < 1e-10,
            "Only FK5 equinox J2000 is supported"
        );
        for key in ["CRPIX1", "CRPIX2", "CRVAL1", "CRVAL2"] {
            ensure!(
                h.get(key)
                    .and_then(Value::as_f64)
                    .is_some_and(f64::is_finite),
                "Missing/non-finite {key}"
            );
        }
        h.insert("NAXIS".into(), json!(2));
        h.insert("NAXIS1".into(), json!(width));
        h.insert("NAXIS2".into(), json!(height));
        // Do the FK5 J2000 frame bias ourselves; the crate's CUSTOM frame conversions
        // are identity TODOs. Never claim those unimplemented transforms are supported.
        h.insert("RADESYS".into(), json!("ICRS"));
        h.remove("EQUINOX");
        h.remove("RADECSYS");
        let matrix = linear_matrix(&Value::Object(h.clone()))?;
        let scale = matrix[0].hypot(matrix[2]).min(matrix[1].hypot(matrix[3]));
        let determinant = matrix[0] * matrix[3] - matrix[1] * matrix[2];
        ensure!(
            matrix.iter().all(|v| v.is_finite()) && scale > 0. && determinant.abs() > 1e-30,
            "Singular/non-finite celestial pixel matrix"
        );
        // Canonical CD avoids mixed CD+PC defaults and permits column-wise native scale.
        for key in [
            "PC1_1", "PC1_2", "PC2_1", "PC2_2", "CDELT1", "CDELT2", "CROTA1", "CROTA2",
        ] {
            h.remove(key);
        }
        for (key, value) in ["CD1_1", "CD1_2", "CD2_1", "CD2_2"].into_iter().zip(matrix) {
            h.insert(key.into(), json!(value));
        }
        let wcs = crate::sky::SkyWcs::new(&Value::Object(h.clone()), width, height)?;
        h.insert("RADESYS".into(), json!(if fk5 { "FK5" } else { "ICRS" }));
        if fk5 {
            h.insert("EQUINOX".into(), json!(2000.));
        }
        Ok(Self {
            wcs,
            header: Value::Object(h),
            scale,
            fk5,
        })
    }

    fn scaled(&self, factor: f64, width: u32, height: u32) -> Result<Self> {
        let mut h = self.header.clone();
        for key in ["CRPIX1", "CRPIX2"] {
            h[key] = json!((number(&h, key, 0.) - 0.5) * factor + 0.5);
        }
        for key in ["CD1_1", "CD1_2", "CD2_1", "CD2_2"] {
            h[key] = json!(number(&h, key, 0.) / factor);
        }
        Self::new(&h, width, height)
    }
}

pub(crate) fn linear_matrix(h: &Value) -> Result<[f64; 4]> {
    if ["CD1_1", "CD1_2", "CD2_1", "CD2_2"]
        .iter()
        .any(|k| h.get(k).is_some())
    {
        return Ok([
            number(h, "CD1_1", 0.),
            number(h, "CD1_2", 0.),
            number(h, "CD2_1", 0.),
            number(h, "CD2_2", 0.),
        ]);
    }
    let (a, b) = (number(h, "CDELT1", 1.), number(h, "CDELT2", 1.));
    if h.get("CROTA2").is_some() && !h.as_object().unwrap().keys().any(|k| k.starts_with("PC")) {
        let r = number(h, "CROTA2", 0.).to_radians();
        return Ok([a * r.cos(), -b * r.sin(), a * r.sin(), b * r.cos()]);
    }
    Ok([
        a * number(h, "PC1_1", 1.),
        a * number(h, "PC1_2", 0.),
        b * number(h, "PC2_1", 0.),
        b * number(h, "PC2_2", 1.),
    ])
}

// IAU/Hipparcos FK5 J2000 -> ICRS orientation; transpose reverses direction.
// Numerical constants are a reference-frame rotation, not translated library code.
pub(crate) fn frame_bias(p: fitsrs::LonLat, to_fk5: bool) -> fitsrs::LonLat {
    const M: [[f64; 3]; 3] = [
        [
            0.9999999999999928,
            -1.110223329740834e-7,
            -4.411804498096776e-8,
        ],
        [
            1.1102233723050031e-7,
            0.9999999999999891,
            9.64779225407974e-8,
        ],
        [
            4.411803426976324e-8,
            -9.647792743888517e-8,
            0.9999999999999943,
        ],
    ];
    let (lon, lat) = (p.lon(), p.lat());
    let v = [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()];
    let mut q = [0.; 3];
    for i in 0..3 {
        for j in 0..3 {
            q[i] += if to_fk5 { M[i][j] } else { M[j][i] } * v[j];
        }
    }
    fitsrs::LonLat::new(
        q[1].atan2(q[0]).rem_euclid(std::f64::consts::TAU),
        q[2].atan2(q[0].hypot(q[1])),
    )
}

struct Raster {
    width: u32,
    height: u32,
    values: Vec<f32>,
}

fn reproject(
    source: &Raster,
    source_wcs: &Celestial,
    target: &Celestial,
    width: u32,
    height: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    reproject_region(
        source,
        source_wcs,
        target,
        height,
        window::Region {
            x: 0,
            y: 0,
            width,
            height,
        },
        cancelled,
    )
}

fn reproject_region(
    source: &Raster,
    source_wcs: &Celestial,
    target: &Celestial,
    full_height: u32,
    region: window::Region,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    reproject_observed(
        (source, source_wcs),
        target,
        full_height,
        region,
        cancelled,
        &mut |_| Ok(()),
    )
}

fn reproject_observed(
    (source, source_wcs): (&Raster, &Celestial),
    target: &Celestial,
    full_height: u32,
    region: window::Region,
    cancelled: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u32) -> Result<()>,
) -> Result<Vec<f32>> {
    let (width, height) = (region.width, region.height);
    let n = count(width, height)?;
    ensure!(
        n <= MAX_CANVAS_PIXELS,
        "Scaled canvas exceeds bounded native preparation limit"
    );
    let mut out = vec![f32::NAN; n];
    for y in 0..height {
        check(cancelled)?;
        for x in 0..width {
            // fitsrs/mapproj use FITS one-based pixel centres. On-disk prepared planes
            // use editor top-down order. The source remains FITS bottom-up.
            let Some(mut sky) = target.wcs.unproj(&fitsrs::ImgXY::new(
                (x + region.x) as f64 + 1.,
                (full_height - y - region.y) as f64,
            )) else {
                continue;
            };
            if target.fk5 != source_wcs.fk5 {
                sky = frame_bias(sky, source_wcs.fk5);
            }
            let Some(p) = source_wcs.wcs.proj(&sky) else {
                continue;
            };
            let (mut sx, mut sy) = (p.x() - 1., p.y() - 1.);
            // Eliminate roundoff at exact centres; NaN neighbors with zero mathematical
            // interpolation weight must not poison a valid exact-centre science sample.
            if (sx - sx.round()).abs() < 1e-7 {
                sx = sx.round();
            }
            if (sy - sy.round()).abs() < 1e-7 {
                sy = sy.round();
            }
            if !sx.is_finite()
                || !sy.is_finite()
                || sx < -0.5
                || sy < -0.5
                || sx > source.width as f64 - 0.5
                || sy > source.height as f64 - 0.5
            {
                continue;
            }
            sx = sx.clamp(0., source.width as f64 - 1.);
            sy = sy.clamp(0., source.height as f64 - 1.);
            let (ix, iy) = (sx.floor() as usize, sy.floor() as usize);
            let (jx, jy) = (
                (ix + 1).min(source.width as usize - 1),
                (iy + 1).min(source.height as usize - 1),
            );
            let (fx, fy) = (sx - ix as f64, sy - iy as f64);
            let mut sum = 0.;
            let mut valid = true;
            for (xx, yy, w) in [
                (ix, iy, (1. - fx) * (1. - fy)),
                (jx, iy, fx * (1. - fy)),
                (ix, jy, (1. - fx) * fy),
                (jx, jy, fx * fy),
            ] {
                if w == 0. {
                    continue;
                }
                let v = source.values[yy * source.width as usize + xx];
                if !v.is_finite() {
                    valid = false;
                    break;
                }
                sum += v as f64 * w;
            }
            if valid {
                out[y as usize * width as usize + x as usize] = sum as f32;
            }
        }
        progress(y + 1)?;
    }
    Ok(out)
}

fn required(meta: &Value, state: &Value) -> Result<(Vec<Value>, HashSet<String>)> {
    let layers = meta["layers"]
        .as_array()
        .context("Project has no source layers")?;
    let all: HashSet<String> = layers
        .iter()
        .filter_map(|l| l["key"].as_str().map(str::to_owned))
        .collect();
    let mut keys = HashSet::new();
    let mut visible = HashSet::new();
    let mut spectral = false;
    for layer in state["layers"]
        .as_array()
        .context("Edit state has no layers")?
    {
        if layer["visible"].as_bool() == Some(false) {
            continue;
        }
        let source = layer["source"]
            .as_str()
            .context("Visible layer has no source")?;
        visible.insert(source.to_owned());
        keys.insert(source.to_owned());
        if let Some(source) = layer["mask"]["source"].as_str() {
            keys.insert(source.to_owned());
        }
        if layer["mode"].as_str() == Some("spectral") {
            spectral = true;
        }
    }
    ensure!(
        !visible.is_empty(),
        "Make at least one source layer visible before exporting."
    );
    let combined_scale = spectral || visible.iter().any(|k| !all.contains(k));
    if spectral || keys.iter().any(|k| !all.contains(k)) {
        keys.extend(all.iter().cloned());
    }
    if combined_scale {
        visible.extend(all);
    }
    Ok((
        layers
            .iter()
            .filter(|l| l["key"].as_str().is_some_and(|k| keys.contains(k)))
            .cloned()
            .collect(),
        visible,
    ))
}

fn source_path(session: &Path, tile: &Value) -> Result<PathBuf> {
    crate::projects::science_source(
        session,
        tile["source"]
            .as_str()
            .context("Missing native science source provenance")?,
    )
}

fn survey_normalization(layer: &Value) -> Result<Option<(f64, f64)>> {
    if crate::discovery::survey_id(layer).is_none()
        || (layer["stats"].get("background").is_none() && layer["stats"].get("scale").is_none())
    {
        return Ok(None);
    }
    let offset = layer["stats"]["background"]
        .as_f64()
        .context("Missing stored survey background")?;
    let scale = layer["stats"]["scale"]
        .as_f64()
        .context("Missing stored survey scale")?;
    ensure!(
        offset.is_finite() && scale.is_finite() && scale > 0.,
        "Invalid stored survey normalization"
    );
    // Flat or empty survey fields cannot be tone-fitted, but can have source-paint edits.
    Ok(Some((offset, scale)))
}

/// Estimate native output from actual available visible FITS source sampling.
/// estimated_s reuses Python's documented empirical non-star work model, not a Rust benchmark.
pub fn plan(session_dir: &Path, state: &Value, long_side: Option<u32>) -> Result<ExportPlan> {
    let meta = read_json(&session_dir.join("project/meta.json"))?;
    let (bw, bh) = (dimension(&meta, "width")?, dimension(&meta, "height")?);
    let grid = Celestial::new(&meta["wcs"], bw, bh)?;
    let (needed, visible) = required(&meta, state)?;
    let mut min_scale = f64::INFINITY;
    let mut fixed = 0.;
    let mut sources_fit = true;
    for layer in &needed {
        let tiles = layer["tiles"].as_array().filter(|t| !t.is_empty());
        let tiles: Vec<&Value> = tiles.map_or_else(|| vec![layer], |t| t.iter().collect());
        for tile in &tiles {
            let science = Science::open(&source_path(session_dir, tile)?)?;
            let source_pixels = count(science.image.width, science.image.height)?;
            sources_fit &= source_pixels <= MAX_SOURCE_PIXELS;
            if visible.contains(layer["key"].as_str().unwrap_or("")) {
                min_scale = min_scale.min(science.wcs.scale);
            }
            fixed += source_pixels as f64 / 1e6 * 0.122;
        }
        if layer["tiles"].as_array().is_some_and(|t| !t.is_empty()) {
            fixed += tiles.len() as f64 * 5.6;
        }
    }
    ensure!(min_scale.is_finite(), "No available native source sampling");
    let native = (grid.scale / min_scale).max(1.);
    let (pw, ph) = crate::geometry::view_dimensions(&meta, state)?;
    let maximum = ((pw.max(ph) as f64 * native) + 0.5).floor();
    ensure!(
        maximum <= u32::MAX as f64,
        "Native output dimensions overflow"
    );
    // Metadata-only admission estimate. Do not load SCI/WHT or cap the true
    // native label: max_long_side is a separate, honest preparation limit.
    let fits = |side: u32| -> bool {
        let factor = native.min(side as f64 / pw.max(ph) as f64);
        if !sources_fit {
            return false;
        }
        preparation_windows(&meta, state, factor)
            .and_then(|windows| windows.validate(needed.len()))
            .is_ok()
    };
    // Zero explicitly means source-backed preparation is unavailable within
    // these budgets. The separate cached renderer remains a quick-export path.
    let (mut low, mut high) = (0_u32, maximum as u32);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if fits(middle) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let max_long_side = Some(low);
    let factor = if let Some(side) = long_side {
        ensure!(
            side > 0 && side as f64 <= maximum,
            "long_side must be positive and no larger than native ({maximum})"
        );
        native.min(side as f64 / pw.max(ph) as f64)
    } else {
        native
    };
    let size = |v: u32| -> Result<u32> {
        let n = (v as f64 * factor + 0.5).floor().max(1.);
        ensure!(n <= u32::MAX as f64, "Native output dimensions overflow");
        Ok(n as u32)
    };
    // First preparation still visits the full grid for exact preview normalization
    // and coverage topology, but retains only the source/render windows.
    let scaled = bw as f64 * bh as f64 * factor * factor / 1e6 * needed.len() as f64 * 0.655;
    Ok(ExportPlan {
        factor,
        native_factor: native,
        width: size(pw)?,
        height: size(ph)?,
        preview_width: pw,
        preview_height: ph,
        estimated_s: fixed + scaled,
        max_long_side,
    })
}

/// Render source-backed crop-local float32 preparation, reusing the latest valid
/// preparation across colour/tone/format changes. Originals are never modified.
pub fn render_export(
    session_dir: &Path,
    state: &Value,
    long_side: Option<u32>,
    cancelled: &dyn Fn() -> bool,
) -> Result<RenderedImage> {
    check(cancelled)?;
    let info = plan(session_dir, state, long_side)?;
    check(cancelled)?;
    // Full export always prepares SCI at the requested output resolution.
    // Tone/blend then RGB downsampling is not equivalent to downsampling
    // science then tone/blend. Cached quick export is render_project directly;
    // never fall back there after a native preparation error.
    ensure!(
        info.max_long_side
            .is_some_and(|maximum| info.width.max(info.height) <= maximum),
        "Requested native export exceeds source/canvas/aggregate allocation budgets; maximum supported long side is {}. No cached enlargement was used.",
        info.max_long_side.unwrap_or(0)
    );
    let original = read_json(&session_dir.join("project/meta.json"))?;
    let windows = preparation_windows(&original, state, info.factor)?;
    let scratch = export_cache(session_dir)?;
    let mut fingerprints = fingerprint::Fingerprints::from_cache(&scratch)?;
    let identity = window::identity(
        session_dir,
        &original,
        state,
        &info,
        &windows,
        &mut fingerprints,
        cancelled,
    )?;
    let dimensions = (info.width, info.height);
    if let Some(prepared) = window::cached(&scratch, &identity)? {
        let rendered =
            crate::render::render_project_with_dimensions(&prepared, state, dimensions, cancelled)?;
        check(cancelled)?;
        ensure!(
            identity
                == window::identity(
                    session_dir,
                    &original,
                    state,
                    &info,
                    &windows,
                    &mut fingerprints,
                    cancelled
                )?
                && original == read_json(&session_dir.join("project/meta.json"))?,
            "Native source/project changed during cached rendering; no stale-source export was returned."
        );
        window::refresh_fingerprints(&scratch, &identity, &fingerprints, cancelled)?;
        return Ok(rendered);
    }
    let temp = tempfile::Builder::new()
        .prefix("native-")
        .tempdir_in(&scratch)
        .context("Cannot create native export scratch directory")?;
    let meta = prepare_window_project(session_dir, state, &info, &windows, temp.path(), cancelled)?;
    fs::write(temp.path().join("meta.json"), serde_json::to_vec(&meta)?)?;
    check(cancelled)?;
    // The planner scales already-rounded preview dimensions. Rounding the
    // physical native crop again can differ on either axis; sample its unchanged
    // sky frame directly onto the exact planned lattice instead.
    let rendered =
        crate::render::render_project_with_dimensions(temp.path(), state, dimensions, cancelled)?;
    ensure!(
        rendered.width == info.width && rendered.height == info.height,
        "Scaled crop rounding is unsupported for this output (planned {}x{}, renderer {}x{}); no preview enlargement was used",
        info.width,
        info.height,
        rendered.width,
        rendered.height
    );
    check(cancelled)?;
    ensure!(
        identity
            == window::identity(
                session_dir,
                &original,
                state,
                &info,
                &windows,
                &mut fingerprints,
                cancelled
            )?
            && original == read_json(&session_dir.join("project/meta.json"))?,
        "Native source/project changed during preparation; no partial cache was published."
    );
    window::publish(&scratch, temp, &identity, &fingerprints, cancelled)?;
    Ok(rendered)
}

fn prepare_project(
    session: &Path,
    state: &Value,
    info: &ExportPlan,
    destination: &Path,
    cancelled: &dyn Fn() -> bool,
) -> Result<Value> {
    let folder = session.join("project");
    let mut meta = read_json(&folder.join("meta.json"))?;
    let original_extent = crate::geometry::canvas_extent(&meta)?;
    let (bw, bh) = (dimension(&meta, "width")?, dimension(&meta, "height")?);
    let (w, h) = (
        (bw as f64 * info.factor + 0.5).floor().max(1.) as u32,
        (bh as f64 * info.factor + 0.5).floor().max(1.) as u32,
    );
    let n = count(w, h)?;
    let (needed, _) = required(&meta, state)?;
    validate_work_budget(n, needed.len())?;
    let grid = Celestial::new(&meta["wcs"], bw, bh)?;
    let mut target = grid.scaled(info.factor, w, h)?;
    let top_down_shift = h as f64 - bh as f64 * info.factor;
    if top_down_shift != 0. {
        // Python subgrid(..., origin=(0, 0), shape=(h, w)) preserves the
        // physical top edge, not the rounded FITS bottom edge. Without this
        // reference-pixel shift, every top-down science sample moves by the
        // fractional difference between allocated and physical canvas height.
        target.header["CRPIX2"] = json!(number(&target.header, "CRPIX2", 0.) + top_down_shift);
        target = Celestial::new(&target.header, w, h)?;
    }
    let mut layers = Vec::new();
    let mut lum = vec![0_f32; n];
    let mut counts = vec![0_u16; n];
    for (index, mut layer) in needed.into_iter().enumerate() {
        check(cancelled)?;
        let raw = if layer["tiles"].as_array().is_some_and(|t| !t.is_empty()) {
            reproject_mosaic(session, &layer, &target, w, h, info.factor, cancelled)?
        } else {
            let source = Science::open(&source_path(session, &layer)?)?;
            let block = (target.scale / source.wcs.scale + 1e-8).floor().max(1.) as u32;
            let (pixels, wcs) = source.prepared(block, cancelled)?;
            reproject(&pixels, &wcs, &target, w, h, cancelled)?
        };
        let stored_mosaic_tone = layer["tiles"].as_array().is_some_and(|t| !t.is_empty())
            && layer["stats"].get("background").is_some();
        let survey_tone = survey_normalization(&layer)?;
        let (a, b) = if let Some(tone) = survey_tone {
            tone
        } else if stored_mosaic_tone {
            (
                number(&layer["stats"], "background", 0.),
                number(&layer["stats"], "scale", 1.),
            )
        } else {
            let small = area_preview(&raw, w, h, info.factor, bw, bh, cancelled)?;
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
        ensure!(
            a.is_finite() && b.is_finite() && b > 0.,
            "Invalid stored source normalization"
        );
        let file = format!("native-{index}.f32");
        let coverage_file = format!("native-{index}.coverage.u8");
        let mut pixels = BufWriter::new(File::create(destination.join(&file))?);
        let mut coverage = BufWriter::new(File::create(destination.join(&coverage_file))?);
        for (i, v) in raw.iter().enumerate() {
            if i % 16384 == 0 {
                check(cancelled)?;
            }
            let covered = v.is_finite();
            let value = if covered {
                ((*v - a as f32) / b as f32).clamp(-65504., 65504.)
            } else {
                0.
            };
            pixels.write_all(&value.to_le_bytes())?;
            coverage.write_all(&[u8::from(covered)])?;
            if covered {
                lum[i] += value;
                counts[i] += 1;
            }
        }
        pixels.flush()?;
        coverage.flush()?;
        layer["file"] = json!(file);
        layer
            .as_object_mut()
            .context("Invalid layer metadata")?
            .remove("starless");
        layer["coverage_file"] = json!(coverage_file);
        layer["native_normalization"] = json!({"offset":a,"scale":b,
            "method":if survey_tone.is_some() {"persisted survey background/scale"} else if stored_mosaic_tone {"persisted mosaic background/scale"} else {"preview robust raw fit"}});
        // Let the renderer compute native edges from actual native coverage.
        if let Some(l) = layer.as_object_mut() {
            l.remove("edge_file");
        }
        layers.push(layer);
    }
    let mut file = BufWriter::new(File::create(destination.join("luminance.f32"))?);
    for (i, (v, c)) in lum.into_iter().zip(counts).enumerate() {
        if i % 16384 == 0 {
            check(cancelled)?;
        }
        file.write_all(&(if c > 0 { v / c as f32 } else { 0. }).to_le_bytes())?;
    }
    file.flush()?;
    meta["width"] = json!(w);
    meta["height"] = json!(h);
    // Buffer dimensions are rounded; normalized geometry uses the exact scaled
    // original extent, matching Python's source-backed full-export sampling.
    meta["_canvas_extent"] = json!(original_extent.map(|v| v * info.factor));
    meta["layers"] = json!(layers);
    meta["wcs"] = target.header;
    meta["pixel_scale_arcsec"] = json!(target.scale * 3600.);
    meta["_detail_scale"] = json!(info.factor);
    meta["luminance"] = json!({"file":"luminance.f32"});
    meta.as_object_mut()
        .context("Invalid project metadata")?
        .remove("stars");
    meta["_native_export"] = json!({"source_backed":true,
        "factor":info.factor,"estimate_model":"Python empirical non-star source/work model; not Rust timing"});
    Ok(meta)
}

fn local_file(folder: &Path, name: &str) -> Result<PathBuf> {
    let p = Path::new(name);
    ensure!(
        p.components().count() == 1
            && !p.is_absolute()
            && matches!(p.components().next(), Some(std::path::Component::Normal(_))),
        "Unsafe built plane filename"
    );
    Ok(folder.join(p))
}

fn read_half(
    folder: &Path,
    name: &Value,
    w: u32,
    h: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    check(cancelled)?;
    let bytes = fs::read(local_file(
        folder,
        name.as_str().context("Missing built science plane")?,
    )?)?;
    ensure!(
        bytes.len() == count(w, h)? * 2,
        "Invalid built f16 science plane size"
    );
    Ok(bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| f16::from_bits(u16::from_le_bytes(*p)).to_f32())
        .collect())
}

fn area_preview(
    raw: &[f32],
    w: u32,
    h: u32,
    factor: f64,
    bw: u32,
    bh: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let mut out = vec![f32::NAN; count(bw, bh)?];
    for y in 0..bh {
        check(cancelled)?;
        for x in 0..bw {
            let (x0, x1, y0, y1) = (
                x as f64 * factor,
                (x + 1) as f64 * factor,
                y as f64 * factor,
                (y + 1) as f64 * factor,
            );
            if x1 > w as f64 + 1e-9 || y1 > h as f64 + 1e-9 {
                continue;
            }
            let mut sums = 0.;
            let mut counts = 0.;
            for yy in y0.floor() as usize..y1.ceil() as usize {
                for xx in x0.floor() as usize..x1.ceil() as usize {
                    let area = ((xx + 1) as f64).min(x1).sub_max(x0.max(xx as f64))
                        * ((yy + 1) as f64).min(y1).sub_max(y0.max(yy as f64));
                    let value = raw[yy * w as usize + xx];
                    if value.is_finite() {
                        sums += value as f64 * area;
                        counts += area;
                    }
                }
            }
            if counts > factor * factor * 0.98 {
                out[y as usize * bw as usize + x as usize] = (sums / counts) as f32;
            }
        }
    }
    Ok(out)
}

trait PositiveDifference {
    fn sub_max(self, other: Self) -> Self;
}
impl PositiveDifference for f64 {
    fn sub_max(self, other: Self) -> Self {
        (self - other).max(0.)
    }
}

fn percentile(values: &[f64], q: f64) -> f64 {
    let mut values = values.to_vec();
    values.sort_unstable_by(f64::total_cmp);
    let p = (values.len() - 1) as f64 * q;
    let lo = p.floor() as usize;
    let hi = p.ceil() as usize;
    values[lo] * (1. - (p - lo as f64)) + values[hi] * (p - lo as f64)
}

fn fit_tone(raw: &[f32], preview: &[f32], valid: &[bool]) -> Result<(f64, f64)> {
    let mut samples: Vec<(f64, f64)> = preview
        .iter()
        .zip(raw)
        .zip(valid)
        .filter(|((x, y), v)| **v && x.is_finite() && y.is_finite())
        .map(|((x, y), _)| (*x as f64, *y as f64))
        .collect();
    ensure!(
        samples.len() >= 8,
        "Insufficient overlapping tonal range to match native source to preview"
    );
    let stride = (samples.len() / 200_000).max(1);
    samples = samples.into_iter().step_by(stride).collect();
    let xs: Vec<_> = samples.iter().map(|p| p.0).collect();
    let ys: Vec<_> = samples.iter().map(|p| p.1).collect();
    let ptp = |v: &[f64]| {
        v.iter().copied().fold(f64::NEG_INFINITY, f64::max)
            - v.iter().copied().fold(f64::INFINITY, f64::min)
    };
    ensure!(
        ptp(&xs) >= 1e-8,
        "Insufficient overlapping tonal range to match native source to preview"
    );
    let (lo, hi) = (percentile(&xs, 0.02), percentile(&xs, 0.95));
    let mut keep: Vec<_> = xs.iter().map(|x| *x >= lo && *x <= hi).collect();
    let (mut a, mut b) = (0., 0.);
    for _ in 0..6 {
        let n = keep.iter().filter(|v| **v).count();
        ensure!(n >= 8, "Insufficient robust tonal samples");
        let (mut sx, mut sy) = (0., 0.);
        for ((x, y), k) in samples.iter().zip(&keep) {
            if *k {
                sx += x;
                sy += y;
            }
        }
        let (mx, my) = (sx / n as f64, sy / n as f64);
        let (mut covariance, mut variance) = (0., 0.);
        for ((x, y), k) in samples.iter().zip(&keep) {
            if *k {
                covariance += (x - mx) * (y - my);
                variance += (x - mx).powi(2);
            }
        }
        b = covariance / variance;
        a = my - b * mx;
        let residual: Vec<_> = samples.iter().map(|(x, y)| y - a - b * x).collect();
        let r: Vec<_> = residual
            .iter()
            .zip(&keep)
            .filter(|(_, k)| **k)
            .map(|(r, _)| *r)
            .collect();
        let median = percentile(&r, 0.5);
        let sigma = (1.4826
            * percentile(
                &r.iter().map(|r| (r - median).abs()).collect::<Vec<_>>(),
                0.5,
            ))
        .max(ptp(&ys) * 1e-7)
        .max(1e-12);
        let next: Vec<_> = samples
            .iter()
            .zip(residual)
            .map(|((x, _), r)| *x >= lo && *x <= hi && (r - median).abs() < 3. * sigma)
            .collect();
        if next.iter().filter(|v| **v).count() < 8 {
            break;
        }
        keep = next;
    }
    ensure!(
        a.is_finite() && b.is_finite() && b > 0.,
        "The native source cannot be tone-matched to the preview"
    );
    Ok((a, b))
}

fn block_mean(source: &Raster, block: u32, cancelled: &dyn Fn() -> bool) -> Result<Raster> {
    let (w, h) = (source.width.div_ceil(block), source.height.div_ceil(block));
    let mut values = vec![f32::NAN; count(w, h)?];
    for y in 0..h {
        check(cancelled)?;
        for x in 0..w {
            let (mut sum, mut n) = (0_f64, 0_u32);
            for yy in y * block..((y + 1) * block).min(source.height) {
                for xx in x * block..((x + 1) * block).min(source.width) {
                    let value = source.values[yy as usize * source.width as usize + xx as usize];
                    if value.is_finite() {
                        sum += value as f64;
                        n += 1;
                    }
                }
            }
            if n > 0 {
                values[y as usize * w as usize + x as usize] = (sum / n as f64) as f32;
            }
        }
    }
    Ok(Raster {
        width: w,
        height: h,
        values,
    })
}

/// Binary dilation/erosion with SciPy's default outside=false boundary.
fn morphology(
    mask: &[bool],
    w: usize,
    h: usize,
    dilate: bool,
    square: bool,
    iterations: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<bool>> {
    let mut result = mask.to_vec();
    for _ in 0..iterations {
        let mut next = vec![!dilate; mask.len()];
        for y in 0..h {
            check(cancelled)?;
            for x in 0..w {
                let mut v = !dilate;
                for dy in -1_isize..=1 {
                    for dx in -1_isize..=1 {
                        if !square && dx != 0 && dy != 0 {
                            continue;
                        }
                        let (xx, yy) = (x as isize + dx, y as isize + dy);
                        let neighbor = xx >= 0
                            && yy >= 0
                            && xx < w as isize
                            && yy < h as isize
                            && result[yy as usize * w + xx as usize];
                        if dilate {
                            v |= neighbor;
                        } else {
                            v &= neighbor;
                        }
                    }
                }
                next[y * w + x] = v;
            }
        }
        result = next;
    }
    Ok(result)
}

fn components(
    mask: &[bool],
    w: usize,
    h: usize,
    eight: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<Vec<usize>>> {
    let mut visited = vec![false; mask.len()];
    let mut result = Vec::new();
    for start in 0..mask.len() {
        if start % 16384 == 0 {
            check(cancelled)?;
        }
        if !mask[start] || visited[start] {
            continue;
        }
        let mut component = Vec::new();
        let mut queue = VecDeque::from([start]);
        visited[start] = true;
        while let Some(p) = queue.pop_front() {
            if component.len() % 16384 == 0 {
                check(cancelled)?;
            }
            component.push(p);
            let (x, y) = (p % w, p / w);
            for dy in -1_isize..=1 {
                for dx in -1_isize..=1 {
                    if (dx == 0 && dy == 0) || (!eight && dx != 0 && dy != 0) {
                        continue;
                    }
                    let (xx, yy) = (x as isize + dx, y as isize + dy);
                    if xx < 0 || yy < 0 || xx >= w as isize || yy >= h as isize {
                        continue;
                    }
                    let q = yy as usize * w + xx as usize;
                    if mask[q] && !visited[q] {
                        visited[q] = true;
                        queue.push_back(q);
                    }
                }
            }
        }
        result.push(component);
    }
    Ok(result)
}

fn weight_masks(
    weight: &[f32],
    w: usize,
    h: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<(Vec<bool>, Vec<bool>)>> {
    let mut sample = Vec::new();
    for y in (0..h).step_by(4) {
        check(cancelled)?;
        for x in (0..w).step_by(4) {
            let v = weight[y * w + x];
            if v.is_finite() && v > 0. {
                sample.push(v as f64);
            }
        }
    }
    if sample.is_empty() {
        return Ok(None);
    }
    let threshold = 0.4 * percentile(&sample, 0.5);
    let low: Vec<_> = weight
        .iter()
        .map(|v| !v.is_finite() || (*v as f64) < threshold)
        .collect();
    let eroded = morphology(&low, w, h, false, true, 3, cancelled)?;
    let opened = morphology(&eroded, w, h, true, true, 3, cancelled)?;
    let mut fringe = vec![false; low.len()];
    // Only opened low-weight regions connected to a real outside edge are fringe.
    for component in components(&opened, w, h, false, cancelled)? {
        if component
            .iter()
            .any(|p| *p < w || *p >= w * (h - 1) || *p % w == 0 || *p % w == w - 1)
        {
            for p in component {
                fringe[p] = true;
            }
        }
    }
    Ok(Some((low, fringe)))
}

fn cut_fringe(raster: &mut Raster, weight: &[f32], cancelled: &dyn Fn() -> bool) -> Result<()> {
    let (w, h) = (raster.width as usize, raster.height as usize);
    let Some((_, fringe)) = weight_masks(weight, w, h, cancelled)? else {
        return Ok(());
    };
    let keep: Vec<_> = fringe.iter().map(|v| !*v).collect();
    let keep = morphology(&keep, w, h, false, false, 2, cancelled)?;
    for (i, (v, keep)) in raster.values.iter_mut().zip(keep).enumerate() {
        if i % 16384 == 0 {
            check(cancelled)?;
        }
        if !keep {
            *v = f32::NAN;
        }
    }
    Ok(())
}

/// Exact separable squared Euclidean distance: lower envelope of 1-D parabolas.
/// Implemented from the mathematical distance definition, not translated C.
fn distances(
    mask: &[bool],
    w: usize,
    h: usize,
    pad: bool,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    fn transform(f: &[f64]) -> Vec<f64> {
        let mut sites = vec![0_usize; f.len()];
        let mut breaks = vec![0_f64; f.len() + 1];
        let mut k = 0;
        sites[0] = 0;
        breaks[0] = f64::NEG_INFINITY;
        breaks[1] = f64::INFINITY;
        for q in 1..f.len() {
            let mut v = sites[k];
            let mut s = ((f[q] + (q * q) as f64) - (f[v] + (v * v) as f64)) / (2. * (q - v) as f64);
            while s <= breaks[k] && k > 0 {
                k -= 1;
                v = sites[k];
                s = ((f[q] + (q * q) as f64) - (f[v] + (v * v) as f64)) / (2. * (q - v) as f64);
            }
            k += 1;
            sites[k] = q;
            breaks[k] = s;
            breaks[k + 1] = f64::INFINITY;
        }
        let mut out = vec![0.; f.len()];
        k = 0;
        for (q, out) in out.iter_mut().enumerate() {
            while breaks[k + 1] < q as f64 {
                k += 1;
            }
            *out = (q as f64 - sites[k] as f64).powi(2) + f[sites[k]];
        }
        out
    }
    let p = usize::from(pad);
    let (ww, hh) = (w + 2 * p, h + 2 * p);
    let mut intermediate = vec![0_f32; ww * hh];
    for y in 0..hh {
        check(cancelled)?;
        let row: Vec<_> = (0..ww)
            .map(|x| {
                if x >= p && x < w + p && y >= p && y < h + p && mask[(y - p) * w + x - p] {
                    1e20
                } else {
                    0.
                }
            })
            .collect();
        let row = transform(&row);
        for (x, d) in row.into_iter().enumerate() {
            intermediate[y * ww + x] = d as f32;
        }
    }
    let mut out = vec![0_f32; w * h];
    for x in p..ww - p {
        check(cancelled)?;
        let column: Vec<_> = (0..hh).map(|y| intermediate[y * ww + x] as f64).collect();
        let column = transform(&column);
        for y in p..hh - p {
            out[(y - p) * w + x - p] = column[y].sqrt() as f32;
        }
    }
    Ok(out)
}

fn reflect(index: isize, size: usize) -> usize {
    let period = 2 * size as isize;
    let i = index.rem_euclid(period);
    if i < size as isize {
        i as usize
    } else {
        (period - i - 1) as usize
    }
}

fn gaussian(
    values: &[f32],
    w: usize,
    h: usize,
    sigma: f64,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let radius = (sigma * 4. + 0.5).floor() as isize;
    let mut kernel: Vec<_> = (-radius..=radius)
        .map(|i| (-0.5 * (i as f64 / sigma).powi(2)).exp())
        .collect();
    let sum: f64 = kernel.iter().sum();
    for v in &mut kernel {
        *v /= sum;
    }
    // SciPy runs axis 0 then axis 1 and stores the intermediate in float32.
    let mut intermediate = vec![0.; values.len()];
    let mut out = vec![0.; values.len()];
    for y in 0..h {
        check(cancelled)?;
        for x in 0..w {
            intermediate[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    *k * values[reflect(y as isize + i as isize - radius, h) * w + x] as f64
                })
                .sum::<f64>() as f32;
        }
    }
    for y in 0..h {
        check(cancelled)?;
        for x in 0..w {
            out[y * w + x] = kernel
                .iter()
                .enumerate()
                .map(|(i, k)| {
                    *k * intermediate[y * w + reflect(x as isize + i as isize - radius, w)] as f64
                })
                .sum::<f64>() as f32;
        }
    }
    Ok(out)
}

fn median_filter(
    values: &[f32],
    w: usize,
    h: usize,
    size: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let mut out = vec![0.; values.len()];
    let r = (size / 2) as isize;
    let mut samples = vec![0_f32; size * size];
    for y in 0..h {
        check(cancelled)?;
        for x in 0..w {
            let mut i = 0;
            for dy in -r..=r {
                for dx in -r..=r {
                    samples[i] =
                        values[reflect(y as isize + dy, h) * w + reflect(x as isize + dx, w)];
                    i += 1;
                }
            }
            samples.select_nth_unstable_by(size * size / 2, f32::total_cmp);
            out[y * w + x] = samples[size * size / 2];
        }
    }
    Ok(out)
}

/// Match build.prepare_science's source-resolution thin-seam repair and core protection.
fn repair_streaks(raster: &mut Raster, weight: &[f32], cancelled: &dyn Fn() -> bool) -> Result<()> {
    let (w, h) = (raster.width as usize, raster.height as usize);
    let Some((low, fringe)) = weight_masks(weight, w, h, cancelled)? else {
        return Ok(());
    };
    let expanded = morphology(&fringe, w, h, true, false, 2, cancelled)?;
    let interior: Vec<_> = low
        .iter()
        .zip(&expanded)
        .map(|(low, fringe)| *low && !*fringe)
        .collect();
    let mut streak = vec![false; low.len()];
    for component in components(&interior, w, h, true, cancelled)? {
        let (x0, x1, y0, y1) = bounds(&component, w);
        let extent = (x1 - x0).max(y1 - y0);
        if extent < 16 || component.len() > 12 * extent || component.len() > extent * extent / 4 {
            continue;
        }
        let (cw, ch) = (x1 - x0, y1 - y0);
        let mut local = vec![false; cw * ch];
        for p in &component {
            local[(p / w - y0) * cw + p % w - x0] = true;
        }
        let distance = distances(&local, cw, ch, true, cancelled)?;
        let selected: Vec<_> = distance
            .iter()
            .zip(local)
            .filter(|(_, v)| *v)
            .map(|(d, _)| *d as f64)
            .collect();
        if percentile(&selected, 0.5) <= 2. {
            for p in component {
                streak[p] = true;
            }
        }
    }
    if !streak.iter().any(|v| *v) {
        return Ok(());
    }
    let protected: Vec<_> = low
        .iter()
        .zip(&streak)
        .zip(&fringe)
        .map(|((l, s), f)| *l && !*s && !*f)
        .collect();
    let protected = morphology(&protected, w, h, true, false, 2, cancelled)?;
    let repair = morphology(&streak, w, h, true, false, 2, cancelled)?;
    let repair: Vec<_> = repair
        .iter()
        .zip(&fringe)
        .zip(protected)
        .map(|((r, f), p)| *r && !*f && !p)
        .collect();
    let mut result = raster.values.clone();
    for y in (0..h).step_by(256) {
        for x in (0..w).step_by(256) {
            check(cancelled)?;
            let (ey, ex) = ((y + 256).min(h), (x + 256).min(w));
            if !(y..ey).any(|yy| repair[yy * w + x..yy * w + ex].iter().any(|v| *v)) {
                continue;
            }
            let (y0, x0, y1, x1) = (
                y.saturating_sub(24),
                x.saturating_sub(24),
                (y + 280).min(h),
                (x + 280).min(w),
            );
            let (cw, ch) = (x1 - x0, y1 - y0);
            let mut local = Vec::with_capacity(cw * ch);
            let mut good = Vec::with_capacity(cw * ch);
            let mut low_local = Vec::with_capacity(cw * ch);
            let mut repair_local = Vec::with_capacity(cw * ch);
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let p = yy * w + xx;
                    let v = raster.values[p];
                    local.push(v);
                    good.push(v.is_finite() && !low[p] && !repair[p]);
                    low_local.push(low[p]);
                    repair_local.push(repair[p]);
                }
            }
            let support = gaussian(
                &good
                    .iter()
                    .map(|v| if *v { 1. } else { 0. })
                    .collect::<Vec<_>>(),
                cw,
                ch,
                4.,
                cancelled,
            )?;
            let filled = gaussian(
                &local
                    .iter()
                    .zip(&good)
                    .map(|(v, g)| if *g { *v } else { 0. })
                    .collect::<Vec<_>>(),
                cw,
                ch,
                4.,
                cancelled,
            )?;
            let filled: Vec<_> = filled
                .iter()
                .zip(&support)
                .map(|(v, s)| if *s > 0. { *v / *s } else { *v })
                .collect();
            let mut change: Vec<_> = repair_local
                .iter()
                .zip(&support)
                .map(|(r, s)| *r && *s > 0.)
                .collect();
            let smooth = median_filter(
                &local
                    .iter()
                    .zip(&filled)
                    .map(|(v, f)| if v.is_finite() { *v } else { *f })
                    .collect::<Vec<_>>(),
                cw,
                ch,
                5,
                cancelled,
            )?;
            let blurred = gaussian(&smooth, cw, ch, 8., cancelled)?;
            let contrast: Vec<_> = smooth.iter().zip(blurred).map(|(s, b)| *s - b).collect();
            let residual: Vec<_> = contrast
                .iter()
                .zip(&good)
                .filter(|(_, g)| **g)
                .map(|(r, _)| *r as f64)
                .collect();
            let noise = if residual.is_empty() {
                0.
            } else {
                let median = percentile(&residual, 0.5);
                1.4826
                    * percentile(
                        &residual
                            .iter()
                            .map(|r| (r - median).abs())
                            .collect::<Vec<_>>(),
                        0.5,
                    )
            };
            let eroded = morphology(&low_local, cw, ch, false, false, 3, cancelled)?;
            let light: Vec<_> = contrast
                .iter()
                .zip(eroded)
                .map(|(c, l)| *c as f64 > 6. * noise && l)
                .collect();
            let mut broad = vec![false; cw * ch];
            for component in components(&light, cw, ch, false, cancelled)? {
                if component.len() >= 49 {
                    for p in component {
                        broad[p] = true;
                    }
                }
            }
            let broad = morphology(&broad, cw, ch, true, false, 4, cancelled)?;
            for (c, b) in change.iter_mut().zip(broad) {
                *c &= !b;
            }
            let mut patched: Vec<_> = local
                .iter()
                .zip(&filled)
                .zip(&change)
                .map(|((l, f), c)| if *c { *f } else { *l })
                .collect();
            let median = median_filter(&patched, cw, ch, 7, cancelled)?;
            let mad = median_filter(
                &patched
                    .iter()
                    .zip(&median)
                    .map(|(p, m)| (p - m).abs())
                    .collect::<Vec<_>>(),
                cw,
                ch,
                7,
                cancelled,
            )?;
            for i in 0..patched.len() {
                if change[i] && (patched[i] - median[i]).abs() > 6. * 1.4826 * mad[i] {
                    patched[i] = median[i];
                }
            }
            for yy in y..ey {
                for xx in x..ex {
                    let i = (yy - y0) * cw + xx - x0;
                    if change[i] {
                        result[yy * w + xx] = patched[i];
                    }
                }
            }
        }
    }
    raster.values = result;
    Ok(())
}

fn bounds(component: &[usize], w: usize) -> (usize, usize, usize, usize) {
    let mut b = (w, 0, usize::MAX, 0);
    for p in component {
        let (x, y) = (p % w, p / w);
        b.0 = b.0.min(x);
        b.1 = b.1.max(x + 1);
        b.2 = b.2.min(y);
        b.3 = b.3.max(y + 1);
    }
    b
}

fn reproject_mosaic(
    session: &Path,
    layer: &Value,
    target: &Celestial,
    w: u32,
    h: u32,
    factor: f64,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let n = count(w, h)?;
    let mut total = vec![0_f32; n];
    let mut weights = vec![0_f32; n];
    let radius = (number(layer, "feather_pixels", 64.) * factor).max(1.);
    for tile in layer["tiles"]
        .as_array()
        .context("Missing mosaic source tiles")?
    {
        check(cancelled)?;
        let gain = tile["gain"]
            .as_f64()
            .context("Missing persisted mosaic gain")?;
        let offset = tile["offset"]
            .as_f64()
            .context("Missing persisted mosaic offset")?;
        ensure!(
            gain.is_finite() && gain > 0. && offset.is_finite(),
            "Invalid persisted mosaic calibration"
        );
        let source = Science::open(&source_path(session, tile)?)?;
        let block = (target.scale / source.wcs.scale + 1e-8).floor().max(1.) as u32;
        let (pixels, wcs) = source.prepared(block, cancelled)?;
        let values = reproject(&pixels, &wcs, target, w, h, cancelled)?;
        let mask: Vec<_> = values.iter().map(|v| v.is_finite()).collect();
        let distance = distances(&mask, w as usize, h as usize, true, cancelled)?;
        for i in 0..n {
            if i % 16384 == 0 {
                check(cancelled)?;
            }
            let weight = (distance[i] as f64 / radius).min(1.) as f32;
            if mask[i] {
                total[i] += (values[i] * gain as f32 + offset as f32) * weight;
                weights[i] += weight;
            }
        }
    }
    for i in 0..n {
        total[i] = if weights[i] > 0. {
            total[i] / weights[i]
        } else {
            f32::NAN
        };
    }
    Ok(total)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(super) fn header(width: u32, height: u32, scale: f64) -> Value {
        let a = 19_f64.to_radians();
        json!({"NAXIS": 2, "NAXIS1": width, "NAXIS2": height,
            "CTYPE1": "RA---TAN", "CTYPE2": "DEC--TAN", "CUNIT1": "deg", "CUNIT2": "deg",
            "RADESYS": "ICRS", "CRVAL1": 359.999, "CRVAL2": 30.,
            "CRPIX1": (width as f64 + 1.)/2., "CRPIX2": (height as f64 + 1.)/2.,
            "CD1_1": -scale*a.cos(), "CD1_2": scale*a.sin(),
            "CD2_1": scale*a.sin(), "CD2_2": scale*a.cos()})
    }

    fn cards(entries: Vec<(String, Value)>) -> Vec<u8> {
        let mut out = Vec::new();
        for (key, value) in entries {
            let value = match value {
                Value::String(s) => format!("'{}'", s.replace('\'', "''")),
                Value::Bool(b) => {
                    if b {
                        "T".into()
                    } else {
                        "F".into()
                    }
                }
                n => n.to_string(),
            };
            out.extend(format!("{key:<8}= {value:>20}").as_bytes());
            out.resize(out.len().div_ceil(80) * 80, b' ');
        }
        out.extend(format!("{:<80}", "END").as_bytes());
        out.resize(out.len().div_ceil(2880) * 2880, b' ');
        out
    }

    pub(crate) fn write_fits(path: &Path, values: &[f32], header: &Value, sci: bool) {
        let mut out = Vec::new();
        if sci {
            let mut primary = vec![
                ("SIMPLE".into(), json!(true)),
                ("BITPIX".into(), json!(8)),
                ("NAXIS".into(), json!(0)),
                ("EXTEND".into(), json!(true)),
            ];
            // Design decision: celestial frame may live only in primary; SCI wins conflicts.
            primary.push(("RADESYS".into(), json!("ICRS")));
            out.extend(cards(primary));
        }
        let mut entries = vec![
            (
                if sci { "XTENSION" } else { "SIMPLE" }.into(),
                if sci { json!("IMAGE") } else { json!(true) },
            ),
            ("BITPIX".into(), json!(-32)),
            ("NAXIS".into(), json!(2)),
            ("NAXIS1".into(), header["NAXIS1"].clone()),
            ("NAXIS2".into(), header["NAXIS2"].clone()),
        ];
        if sci {
            entries.extend([
                ("PCOUNT".into(), json!(0)),
                ("GCOUNT".into(), json!(1)),
                ("EXTNAME".into(), json!("SCI")),
            ]);
        }
        entries.extend(
            header
                .as_object()
                .unwrap()
                .iter()
                .filter(|(k, _)| !k.starts_with("NAXIS") && !(sci && *k == "RADESYS"))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        out.extend(cards(entries));
        for value in values {
            out.extend(value.to_be_bytes());
        }
        out.resize(out.len().div_ceil(2880) * 2880, 0);
        std::fs::write(path, out).unwrap();
    }

    fn write_gzip_rows(path: &Path, tile_width: u32) {
        // Fixed gzip streams encode two big-endian i32 rows. The streams are
        // synthetic test data, not copied decompression code or a new dependency.
        let rows = [
            vec![
                31, 139, 8, 0, 0, 0, 0, 0, 2, 10, 251, 255, 255, 255, 31, 6, 8, 96, 5, 226, 20, 0,
                0, 185, 10, 192, 16, 0, 0, 0,
            ],
            vec![
                31, 139, 8, 0, 0, 0, 0, 0, 2, 10, 99, 96, 96, 96, 254, 255, 255, 255, 63, 6, 6, 6,
                22, 32, 62, 1, 0, 10, 230, 31, 162, 16, 0, 0, 0,
            ],
        ];
        let mut bytes = cards(vec![
            ("SIMPLE".into(), json!(true)),
            ("BITPIX".into(), json!(8)),
            ("NAXIS".into(), json!(0)),
            ("EXTEND".into(), json!(true)),
        ]);
        let mut entries = vec![
            ("XTENSION".into(), json!("BINTABLE")),
            ("BITPIX".into(), json!(8)),
            ("NAXIS".into(), json!(2)),
            ("NAXIS1".into(), json!(8)),
            ("NAXIS2".into(), json!(2)),
            (
                "PCOUNT".into(),
                json!(rows.iter().map(Vec::len).sum::<usize>()),
            ),
            ("GCOUNT".into(), json!(1)),
            ("TFIELDS".into(), json!(1)),
            ("TTYPE1".into(), json!("COMPRESSED_DATA")),
            ("TFORM1".into(), json!("1PB")),
            ("EXTNAME".into(), json!("SCI")),
            ("ZIMAGE".into(), json!(true)),
            ("ZCMPTYPE".into(), json!("GZIP_1")),
            ("ZBITPIX".into(), json!(32)),
            ("ZNAXIS".into(), json!(2)),
            ("ZNAXIS1".into(), json!(4)),
            ("ZNAXIS2".into(), json!(2)),
            ("ZTILE1".into(), json!(tile_width)),
            ("ZTILE2".into(), json!(1)),
        ];
        entries.extend(
            header(4, 2, 1. / 3600.)
                .as_object()
                .unwrap()
                .iter()
                .filter(|(k, _)| !k.starts_with("NAXIS"))
                .map(|(k, v)| (k.clone(), v.clone())),
        );
        bytes.extend(cards(entries));
        let mut offset = 0_i32;
        for row in &rows {
            bytes.extend((row.len() as i32).to_be_bytes());
            bytes.extend(offset.to_be_bytes());
            offset += row.len() as i32;
        }
        for row in rows {
            bytes.extend(row);
        }
        bytes.resize(bytes.len().div_ceil(2880) * 2880, 0);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn supported_full_row_gzip_science_and_partial_tiles_rejected_before_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rows.fits");
        write_gzip_rows(&path, 4);
        let source = Science::open(&path).unwrap();
        let pixels = source.load(&|| false).unwrap();
        assert_eq!(pixels.values, vec![-4., 0., 5., 100., 3., -2., 4., 200.]);
        let bad = dir.path().join("partial.fits");
        write_gzip_rows(&bad, 3);
        let error = Science::open(&bad).err().unwrap().to_string();
        assert!(error.contains("rejected before decoding"));
        // Header validation must also run on a direct loader invocation before
        // File::open, allocation or any decompressor pixel is requested.
        let mut info = source.image.clone();
        info.header["ZTILE1"] = json!(3);
        let error = load_image(&dir.path().join("not-present.fits"), &info, &|| false)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("rejected before decoding"));
        info.header["ZTILE1"] = json!(4);
        info.header["ZBITPIX"] = json!(-32);
        let error = load_image(&dir.path().join("not-present.fits"), &info, &|| false)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("Unsupported compressed floating"));
    }

    #[test]
    fn fractional_preview_crops_export_exact_plan_dimensions_and_physical_sky_samples() {
        // Design decision: the public planner rounds preview crop dimensions
        // before scaling. Final sampling uses that exact lattice, not a newly
        // rounded native crop, while retaining the original physical sky frame.
        let session = tempfile::tempdir().unwrap();
        let project = session.path().join("project");
        fs::create_dir(&project).unwrap();
        let source: Vec<f32> = (0..200)
            .flat_map(|y| (0..200).map(move |x| 0.1 + x as f32 * 0.001 + y as f32 * 0.002))
            .collect();
        write_fits(
            &session.path().join("science.fits"),
            &source,
            &header(200, 200, 1. / 3600.),
            true,
        );
        let mut preview = Vec::new();
        for y in 0..100 {
            for x in 0..100 {
                let sy = (99 - y) * 2;
                let mean = (source[sy * 200 + x * 2]
                    + source[sy * 200 + x * 2 + 1]
                    + source[(sy + 1) * 200 + x * 2]
                    + source[(sy + 1) * 200 + x * 2 + 1])
                    / 4.;
                preview.extend(f16::from_f32(mean).to_bits().to_le_bytes());
            }
        }
        fs::write(project.join("science.f16"), preview).unwrap();
        let meta = json!({"width":100,"height":100,"wcs":header(100,100,2./3600.),
            "layers":[{"key":"science","file":"science.f16","source":"science.fits"}]});
        fs::write(project.join("meta.json"), meta.to_string()).unwrap();
        for (crop_height, rotation, flip, side, expected) in [
            (0.105, 0, false, None, (22, 22)),
            (0.155, 0, false, None, (22, 32)),
            (0.155, 90, true, None, (32, 22)),
            (0.155, 0, false, Some(17), (12, 17)),
            (0.155, 90, true, Some(17), (17, 12)),
            (0.155, 0, false, Some(18), (12, 18)),
        ] {
            let state = json!({"layers":[{"source":"science","mode":"tint",
                "color":"#ffffff","stretch":"linear","blend":"add"}],"global":{
                "rotation":rotation,"flipX":flip,
                "crop":{"x":0.3,"y":0.4,"w":0.105,"h":crop_height,"rotation":0}}});
            let info = plan(session.path(), &state, side).unwrap();
            assert_eq!((info.width, info.height), expected);
            let image = render_export(session.path(), &state, side, &|| false).unwrap();
            assert_eq!((image.width, image.height), expected);
            let scratch = tempfile::tempdir().unwrap();
            let prepared =
                prepare_project(session.path(), &state, &info, scratch.path(), &|| false).unwrap();
            let normalization = &prepared["layers"][0]["native_normalization"];
            let a = normalization["offset"].as_f64().unwrap();
            let b = normalization["scale"].as_f64().unwrap();
            fs::write(
                scratch.path().join("meta.json"),
                serde_json::to_vec(&prepared).unwrap(),
            )
            .unwrap();
            let uncapped =
                crate::render::render_project(scratch.path(), &state, None, &|| false).unwrap();
            // The shared geometry oracle supplies sky coordinates on the
            // original preview frame. An affine SCI ramp has exact bilinear
            // samples, independently of the renderer's output-size calculation.
            let view = crate::geometry::View::new(&meta, &state).unwrap();
            // Also exercise the unchanged output-size path: fractional storage
            // must not shift science independently of the planned lattice.
            for image in [&uncapped, &image] {
                for y in 0..image.height {
                    for x in 0..image.width {
                        let ox =
                            view.frame.x + (x as f64 + 0.5) / image.width as f64 * view.frame.w;
                        let oy =
                            view.frame.y + (y as f64 + 0.5) / image.height as f64 * view.frame.h;
                        assert!(view.inside_crop(ox, oy));
                        let (qx, qy) = view.image_point(ox, oy);
                        let raw =
                            0.1 + (qx * 200. - 0.5) * 0.001 + (199. - (qy * 200. - 0.5)) * 0.002;
                        let expected = ((raw - a) / b).clamp(0., 1.) as f32;
                        let index = (y * image.width + x) as usize * 3;
                        for actual in &image.pixels[index..index + 3] {
                            assert!(
                                (actual - expected).abs() < 2e-6,
                                "{actual} != {expected}; factor {}, output {}x{}, plan {}x{}",
                                info.factor,
                                image.width,
                                image.height,
                                info.width,
                                info.height
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn native_preparation_preserves_float32_science_and_luminance_bits() {
        let session = tempfile::tempdir().unwrap();
        let project = session.path().join("project");
        fs::create_dir(&project).unwrap();
        let source: Vec<f32> = (0..96).map(|i| 0.01234567 + i as f32 * 0.00371).collect();
        write_fits(
            &session.path().join("science.fits"),
            &source,
            &header(12, 8, 1. / 3600.),
            true,
        );
        let mut preview = Vec::new();
        for y in 0..4 {
            for x in 0..6 {
                let sy = (3 - y) * 2;
                let mean = (source[sy * 12 + x * 2]
                    + source[sy * 12 + x * 2 + 1]
                    + source[(sy + 1) * 12 + x * 2]
                    + source[(sy + 1) * 12 + x * 2 + 1])
                    / 4.;
                preview.extend(f16::from_f32(mean).to_bits().to_le_bytes());
            }
        }
        fs::write(project.join("science.f16"), preview).unwrap();
        let meta = json!({"width":6,"height":4,"wcs":header(6,4,2./3600.),
            "layers":[{"key":"science","file":"science.f16","source":"science.fits"}]});
        fs::write(project.join("meta.json"), meta.to_string()).unwrap();
        let state = json!({"layers":[{"source":"science","mode":"tint"}],"global":{}});
        let info = plan(session.path(), &state, None).unwrap();
        let prepared = tempfile::tempdir().unwrap();
        let meta =
            prepare_project(session.path(), &state, &info, prepared.path(), &|| false).unwrap();
        let layer = &meta["layers"][0];
        assert!(layer["file"].as_str().unwrap().ends_with(".f32"));
        let bytes = fs::read(prepared.path().join(layer["file"].as_str().unwrap())).unwrap();
        assert_eq!(bytes.len(), 12 * 8 * 4);
        let pixels: Vec<_> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| f32::from_le_bytes(*p))
            .collect();
        let a = layer["native_normalization"]["offset"].as_f64().unwrap() as f32;
        let b = layer["native_normalization"]["scale"].as_f64().unwrap() as f32;
        for y in 0..8 {
            for x in 0..12 {
                let expected = (source[(7 - y) * 12 + x] - a) / b;
                assert_eq!(pixels[y * 12 + x].to_bits(), expected.to_bits());
            }
        }
        // Design decision: native PNG16 must not inherit a half-float precision
        // boundary merely because old cached planes are f16.
        assert!(pixels.iter().any(|v| f16::from_f32(*v).to_f32() != *v));
        assert_eq!(meta["luminance"]["file"], json!("luminance.f32"));
        assert_eq!(
            fs::read(prepared.path().join("luminance.f32")).unwrap(),
            bytes
        );
        assert_eq!(meta["_canvas_extent"], json!([12., 8.]));
        // Design decision: scaled storage dimensions are integers, but normalized
        // geometry must retain the original canvas extent before rounding.
        // Replacing this with the allocated height silently changes final sampling.
        let reduced = plan(session.path(), &state, Some(5)).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let scaled =
            prepare_project(session.path(), &state, &reduced, scratch.path(), &|| false).unwrap();
        assert_eq!(
            (scaled["width"].as_u64(), scaled["height"].as_u64()),
            (Some(5), Some(3))
        );
        assert_eq!(
            scaled["_canvas_extent"],
            json!([6. * reduced.factor, 4. * reduced.factor])
        );
        assert_ne!(scaled["_canvas_extent"][1], scaled["height"]);
        assert_eq!(scaled["_detail_scale"], json!(reduced.factor));
    }

    fn window_fixture(session: &Path) -> Value {
        window_fixture_size(session, 128, 96)
    }

    fn window_fixture_size(session: &Path, bw: u32, bh: u32) -> Value {
        let project = session.join("project");
        fs::create_dir_all(&project).unwrap();
        let source: Vec<f32> = (0..bh * 2)
            .flat_map(|y| {
                (0..bw * 2).map(move |x| {
                    if x < 54 * bw / 128
                        || ((100 * bh / 96..=102 * bh / 96).contains(&y)
                            && (80 * bw / 128..=240 * bw / 128).contains(&x))
                    {
                        f32::NAN
                    } else {
                        0.1 + x as f32 * (128. / bw as f32) * 0.001
                            + y as f32 * (96. / bh as f32) * 0.002
                            + ((x * 13 + y * 7) % 19) as f32 * 0.003
                    }
                })
            })
            .collect();
        write_fits(
            &session.join("science.fits"),
            &source,
            &header(bw * 2, bh * 2, 1. / 3600.),
            true,
        );
        let grid = Celestial::new(&header(bw, bh, 2. / 3600.), bw, bh).unwrap();
        let science = Science::open(&session.join("science.fits")).unwrap();
        let (small, wcs) = science.prepared(2, &|| false).unwrap();
        let preview = reproject(&small, &wcs, &grid, bw, bh, &|| false).unwrap();
        fs::write(
            project.join("science.f16"),
            preview
                .iter()
                .flat_map(|v| {
                    f16::from_f32(if v.is_finite() { *v } else { 0. })
                        .to_bits()
                        .to_le_bytes()
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        fs::write(
            project.join("science.coverage.u8"),
            preview
                .iter()
                .map(|v| u8::from(v.is_finite()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let meta = json!({"width":bw,"height":bh,"wcs":grid.header,
            "layers":[{"key":"science","file":"science.f16","source":"science.fits",
                "coverage_file":"science.coverage.u8","wavelength_nm":550},
                {"key":"mask","file":"science.f16","source":"science.fits",
                "coverage_file":"science.coverage.u8","wavelength_nm":700}]});
        fs::write(project.join("meta.json"), meta.to_string()).unwrap();
        json!({"layers":[{"source":"science","mode":"tint","color":"#de713f",
            "stretch":"linear","blend":"add","white":0.9,
            "detail":{"amount":0.4,"radius":0.8},
            "transform":{"x":0.03,"y":-0.02,"scale":1.07,"rotation":13},
            "clip":{"x":0.15,"y":0.2,"w":0.55,"h":0.6,"feather":0.15,"shape":"ellipse"},
            "edge":{"feather":5,"irregularity":0.6},
            "mask":{"source":"mask","black":0.1,"white":0.8},
            "brush":{"strokes":[{"mode":"hide","size":12,"hardness":0.3,
                "strength":0.6,"points":[[45,40],[65,55]]}]}},
            {"source":"lum","mode":"spectral","opacity":0.15,"stretch":"linear",
                "spectral":{"stops":[{"nm":400,"color":"#4060ff"},{"nm":800,"color":"#ff7040"}]}}],
            "global":{"rotation":37,"flipX":true,
                "crop":{"x":0.61,"y":0.31,"w":0.23,"h":0.265,"rotation":23},
                "output":{"exposure":0.2,"highlights":0.3}}})
    }

    #[test]
    fn output_tile_seams_match_full_fractional_transformed_detail_and_spectral_render() {
        // Design decision: final sampling uses global output pixel indices;
        // tiling must not restart the crop lattice or truncate Gaussian support.
        let session = tempfile::tempdir().unwrap();
        let mut state = window_fixture_size(session.path(), 601, 451);
        state["global"]["crop"] = json!({"x":0,"y":0,"w":1,"h":1});
        state["global"]["rotation"] = json!(13);
        let info = plan(session.path(), &state, Some(1100)).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let full_meta =
            prepare_project(session.path(), &state, &info, scratch.path(), &|| false).unwrap();
        fs::write(scratch.path().join("meta.json"), full_meta.to_string()).unwrap();
        let expected = crate::render::render_project_with_dimensions(
            scratch.path(),
            &state,
            (info.width, info.height),
            &|| false,
        )
        .unwrap();
        let original = read_json(&session.path().join("project/meta.json")).unwrap();
        let windows = preparation_windows(&original, &state, info.factor).unwrap();
        assert!(windows.tiles.len() > 1);
        let prepared = tempfile::tempdir().unwrap();
        let meta = prepare_window_project(
            session.path(),
            &state,
            &info,
            &windows,
            prepared.path(),
            &|| false,
        )
        .unwrap();
        fs::write(prepared.path().join("meta.json"), meta.to_string()).unwrap();
        let actual = crate::render::render_project_with_dimensions(
            prepared.path(),
            &state,
            (info.width, info.height),
            &|| false,
        )
        .unwrap();
        let peak = expected
            .pixels
            .iter()
            .zip(actual.pixels)
            .map(|(a, b)| (a - b).abs())
            .fold(0_f32, f32::max);
        assert!(peak < 2e-6, "tile/full seam drift {peak}");
        for (reference, local) in full_meta["layers"]
            .as_array()
            .unwrap()
            .iter()
            .zip(meta["layers"].as_array().unwrap())
        {
            assert_eq!(
                reference["native_normalization"],
                local["native_normalization"]
            );
        }
    }

    #[test]
    fn crop_windows_match_full_canvas_transformed_detail_masks_and_coverage() {
        // Design decision: storage origins are not new normalized-coordinate anchors.
        // Full-canvas tone fitting and global hole classification remain authoritative.
        let session = tempfile::tempdir().unwrap();
        let mut state = window_fixture(session.path());
        let mut paint = state["layers"][0]["brush"]["strokes"][0].clone();
        paint["mode"] = json!("white");
        paint["strength"] = json!(0.75);
        state["layers"][0]["brush"]["strokes"]
            .as_array_mut()
            .unwrap()
            .push(paint.clone());
        paint["mode"] = json!("black");
        paint["strength"] = json!(0.25);
        state["layers"][0]["brush"]["strokes"]
            .as_array_mut()
            .unwrap()
            .push(paint);
        let meta_path = session.path().join("project/meta.json");
        let mut original = read_json(&meta_path).unwrap();
        original["_canvas_extent"] = json!([127.8, 95.75]);
        fs::write(&meta_path, original.to_string()).unwrap();
        for side in [None, Some(43)] {
            let info = plan(session.path(), &state, side).unwrap();
            let full = tempfile::tempdir().unwrap();
            let meta =
                prepare_project(session.path(), &state, &info, full.path(), &|| false).unwrap();
            fs::write(full.path().join("meta.json"), meta.to_string()).unwrap();
            let expected = crate::render::render_project_with_dimensions(
                full.path(),
                &state,
                (info.width, info.height),
                &|| false,
            )
            .unwrap();
            let local = tempfile::tempdir().unwrap();
            let windows = preparation_windows(
                &read_json(&session.path().join("project/meta.json")).unwrap(),
                &state,
                info.factor,
            )
            .unwrap();
            assert!(
                windows.source.pixels().unwrap()
                    < count(
                        meta["width"].as_u64().unwrap() as u32,
                        meta["height"].as_u64().unwrap() as u32
                    )
                    .unwrap()
            );
            let prepared = prepare_window_project(
                session.path(),
                &state,
                &info,
                &windows,
                local.path(),
                &|| false,
            )
            .unwrap();
            fs::write(local.path().join("meta.json"), prepared.to_string()).unwrap();
            let actual = crate::render::render_project_with_dimensions(
                local.path(),
                &state,
                (info.width, info.height),
                &|| false,
            )
            .unwrap();
            let peak = actual
                .pixels
                .iter()
                .zip(&expected.pixels)
                .map(|(a, b)| (a - b).abs())
                .fold(0_f32, f32::max);
            assert!(peak < 2e-6, "window/full RGB drift: {peak}");
            assert_eq!(
                prepared["layers"][0]["native_normalization"],
                meta["layers"][0]["native_normalization"]
            );
            if side.is_none() {
                let mut no_feather = state.clone();
                no_feather["layers"][0]["edge"]["feather"] = json!(0);
                let flat = crate::render::render_project_with_dimensions(
                    full.path(),
                    &no_feather,
                    (info.width, info.height),
                    &|| false,
                )
                .unwrap();
                let effect = expected
                    .pixels
                    .iter()
                    .zip(flat.pixels)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0_f32, f32::max);
                assert!(
                    effect > 1e-4,
                    "fixture must actually exercise coverage feathering"
                );
            }
        }
    }

    #[test]
    fn prepared_cache_reuses_science_but_invalidates_provenance_and_cleans_cancellation() {
        // Design decision: latest successful preparation is retained until its replacement
        // renders successfully; colours/output encoding do not identify source preparation.
        let library = tempfile::tempdir().unwrap();
        let session = library.path().join("sessions/private");
        let mut state = window_fixture(&session);
        let first = render_export(&session, &state, Some(43), &|| false).unwrap();
        let cache = export_cache(&session).unwrap();
        let latest = read_json(&cache.join("latest.json")).unwrap();
        state["layers"][0]["color"] = json!("#38a7f0");
        state["global"]["output"]["exposure"] = json!(-0.3);
        let second = render_export(&session, &state, Some(43), &|| false).unwrap();
        assert_ne!(first.pixels, second.pixels);
        assert_eq!(latest, read_json(&cache.join("latest.json")).unwrap());
        let cached_calls = std::cell::Cell::new(0);
        let cached_cancel = render_export(&session, &state, Some(43), &|| {
            cached_calls.set(cached_calls.get() + 1);
            cached_calls.get() > 45
        })
        .err()
        .unwrap();
        assert!(cached_cancel.to_string().contains("cancel"));
        assert_eq!(latest, read_json(&cache.join("latest.json")).unwrap());
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
        state["global"]["crop"]["x"] = json!(0.63);
        let calls = std::cell::Cell::new(0);
        let error = render_export(&session, &state, Some(43), &|| {
            calls.set(calls.get() + 1);
            calls.get() > 60
        })
        .err()
        .unwrap();
        assert!(error.to_string().contains("cancel"));
        assert_eq!(latest, read_json(&cache.join("latest.json")).unwrap());
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
        state["global"]["crop"]["x"] = json!(0.61);
        render_export(&session, &state, Some(43), &|| false).unwrap();
        assert_eq!(latest, read_json(&cache.join("latest.json")).unwrap());
        let source_path = session.join("science.fits");
        let source_modified = fs::metadata(&source_path).unwrap().modified().unwrap();
        let mut source = fs::read(&source_path).unwrap();
        // Change a finite source pixel, retaining source length and valid WCS.
        source[5760 + (100 * 256 + 100) * 4..5760 + (100 * 256 + 100) * 4 + 4]
            .copy_from_slice(&0.7321_f32.to_be_bytes());
        fs::write(&source_path, source).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&source_path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(source_modified))
            .unwrap();
        assert_eq!(
            fs::metadata(&source_path).unwrap().modified().unwrap(),
            source_modified
        );
        render_export(&session, &state, Some(43), &|| false).unwrap();
        assert_ne!(latest, read_json(&cache.join("latest.json")).unwrap());
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
        let changed_source = read_json(&cache.join("latest.json")).unwrap();
        render_export(&session, &state, Some(42), &|| false).unwrap();
        assert_ne!(
            changed_source,
            read_json(&cache.join("latest.json")).unwrap()
        );
        let changed_resolution = read_json(&cache.join("latest.json")).unwrap();
        state["layers"][1]["visible"] = json!(false);
        state["layers"][0]["mask"] = Value::Null;
        render_export(&session, &state, Some(42), &|| false).unwrap();
        assert_ne!(
            changed_resolution,
            read_json(&cache.join("latest.json")).unwrap()
        );
        assert_eq!(fs::read_dir(&cache).unwrap().count(), 2);
        fs::remove_file(source_path).unwrap();
        assert!(render_export(&session, &state, Some(43), &|| false).is_err());
    }

    #[test]
    #[ignore = "Private bounded 2K/4K execution; set ASTROIMP_WINDOW_VALIDATION_ROOT and use --release"]
    fn tiled_eight_layer_2k_4k_exports_and_repeated_preparation_timings() {
        // Design decision: reproduce the previously rejected full-canvas/crop
        // allocation ratio with dense analytic source science, not cached RGB.
        let root = PathBuf::from(std::env::var("ASTROIMP_WINDOW_VALIDATION_ROOT").unwrap());
        fs::create_dir_all(&root).unwrap();
        let session = root.join("sessions/window-validation");
        let project = session.join("project");
        fs::create_dir_all(&project).unwrap();
        let (bw, bh, sw, sh) = (3072_u32, 2262_u32, 6144_u32, 4096_u32);
        let source_header = header(sw, sh, 1. / 3600.);
        let source: Vec<f32> = (0..sh)
            .flat_map(|y| {
                (0..sw).map(move |x| {
                    0.08 + x as f32 * 0.00004
                        + y as f32 * 0.00007
                        + ((x * 17 + y * 11) % 23) as f32 * 0.004
                })
            })
            .collect();
        write_fits(&session.join("science.fits"), &source, &source_header, true);
        let science = Science::open(&session.join("science.fits")).unwrap();
        let grid = Celestial::new(&header(bw, bh, 3. / 3600.), bw, bh).unwrap();
        let (pixels, wcs) = science.prepared(3, &|| false).unwrap();
        let built = reproject(&pixels, &wcs, &grid, bw, bh, &|| false).unwrap();
        drop((source, pixels));
        fs::write(
            project.join("science.f16"),
            built
                .iter()
                .flat_map(|v| {
                    f16::from_f32(if v.is_finite() { *v } else { 0. })
                        .to_bits()
                        .to_le_bytes()
                })
                .collect::<Vec<_>>(),
        )
        .unwrap();
        fs::write(
            project.join("science.coverage.u8"),
            built
                .iter()
                .map(|v| u8::from(v.is_finite()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        drop(built);
        let meta = json!({"width":bw,"height":bh,"wcs":grid.header,
            "layers":(0..8).map(|i| json!({"key":format!("s{i}"),"file":"science.f16",
                "coverage_file":"science.coverage.u8","source":"science.fits",
                "wavelength_nm":450+i*100})).collect::<Vec<_>>()});
        fs::write(project.join("meta.json"), meta.to_string()).unwrap();
        let mut state = json!({"layers":(0..8).map(|i| json!({
            "source":format!("s{i}"),"mode":"tint","color":"#ffffff",
            "stretch":"linear","intensity":0.125,"blend":"add",
            "edge":{"feather":47,"irregularity":0.76}
        })).collect::<Vec<_>>(),"global":{"rotation":-39.579242724962455,
            "crop":{"rotation":-39.579242724962455,
                "x":0.0943307933789268,"y":0.3245161981317972,
                "w":0.6041619913814611,"h":0.4615348841986144},"output":{}}});
        let native = plan(&session, &state, None).unwrap();
        assert_eq!((native.width, native.height), (5568, 3132));
        assert!(native.max_long_side.unwrap() >= 4096);
        println!(
            "Synthetic true Native={}x{}, max_long_side={:?}",
            native.width, native.height, native.max_long_side
        );
        for side in [2048, 4096] {
            let info = plan(&session, &state, Some(side)).unwrap();
            let windows = preparation_windows(&meta, &state, info.factor).unwrap();
            if side == 4096 {
                assert!(validate_work_budget(windows.source.pixels().unwrap(), 8).is_err());
                assert!(validate_work_budget(windows.render.pixels().unwrap(), 0).is_err());
            }
            windows.validate(8).unwrap();
            let started = std::time::Instant::now();
            let first = render_export(&session, &state, Some(side), &|| false).unwrap();
            let first_s = started.elapsed().as_secs_f64();
            assert_eq!((first.width, first.height), (side, side * 9 / 16));
            assert!(first.pixels.iter().all(|v| v.is_finite()));
            if side == 4096 {
                let enlarged_preview = crate::render::render_project_with_dimensions(
                    &project,
                    &state,
                    (info.width, info.height),
                    &|| false,
                )
                .unwrap();
                let detail_difference = first
                    .pixels
                    .iter()
                    .zip(enlarged_preview.pixels)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0_f32, f32::max);
                assert!(
                    detail_difference > 0.01,
                    "source sampling must retain detail absent from enlarged cached science"
                );
                println!(
                    "4096 source-detail vs explicitly enlarged cached-plane reference: \
                    max_abs_RGB={detail_difference:.6}"
                );
            }
            let cache = export_cache(&session).unwrap();
            let latest = read_json(&cache.join("latest.json")).unwrap();
            let prepared = cache.join(latest["generation"].as_str().unwrap());
            let prepared_meta = read_json(&prepared.join("meta.json")).unwrap();
            let layer = &prepared_meta["layers"][0];
            let bytes = fs::read(prepared.join(layer["file"].as_str().unwrap())).unwrap();
            assert_eq!(bytes.len(), windows.source.pixels().unwrap() * 4);
            assert!(bytes.as_chunks::<4>().0.iter().any(|b| {
                let v = f32::from_le_bytes(*b);
                v > 0. && v.to_bits() != f16::from_f32(v).to_f32().to_bits()
            }));
            drop(bytes);
            crate::render::write_image(
                &first,
                &root.join(format!("source-{side}.png")),
                "png16",
                92,
            )
            .unwrap();
            let unchanged = std::time::Instant::now();
            let repeated = render_export(&session, &state, Some(side), &|| false).unwrap();
            let repeat_s = unchanged.elapsed().as_secs_f64();
            assert_eq!(first.pixels, repeated.pixels);
            drop(repeated);
            state["layers"][0]["color"] = json!("#4080ff");
            let recolour = std::time::Instant::now();
            let coloured = render_export(&session, &state, Some(side), &|| false).unwrap();
            let colour_s = recolour.elapsed().as_secs_f64();
            assert_ne!(first.pixels, coloured.pixels);
            assert_eq!(latest, read_json(&cache.join("latest.json")).unwrap());
            crate::render::write_image(
                &coloured,
                &root.join(format!("recolour-{side}.jpg")),
                "jpeg",
                92,
            )
            .unwrap();
            println!(
                "eight-source detail {}x{}: full={}x{}, \
                DISK source_window={}x{}, max actual tile science={:.2} MiB (eight f32), \
                max actual tile RGB={:.2} MiB, final RGB={:.2} MiB, \
                first_s={first_s:.3}, repeated_s={repeat_s:.3}, recolour_s={colour_s:.3}; same cache generation, \
                bit-exact repeated RGB, source float32 precision verified, PNG16/JPEG encoded",
                first.width,
                first.height,
                windows.full.width,
                windows.full.height,
                windows.source.width,
                windows.source.height,
                windows
                    .tiles
                    .iter()
                    .map(|t| t.source.pixels().unwrap())
                    .max()
                    .unwrap() as f64
                    * 32.
                    / 1048576.,
                windows
                    .tiles
                    .iter()
                    .map(|t| t.render.pixels().unwrap())
                    .max()
                    .unwrap() as f64
                    * 12.
                    / 1048576.,
                (info.width as f64 * info.height as f64) * 12. / 1048576.,
            );
            state["layers"][0]["color"] = json!("#ffffff");
        }
    }

    #[test]
    fn native_exports_ignore_missing_present_and_malformed_legacy_star_assets() {
        let session = tempfile::tempdir().unwrap();
        let project = session.path().join("project");
        fs::create_dir(&project).unwrap();
        let source: Vec<f32> = (0..64).map(|i| 0.02 + i as f32 * 0.01).collect();
        write_fits(
            &session.path().join("science.fits"),
            &source,
            &header(8, 8, 1. / 3600.),
            true,
        );
        let raw: Vec<f32> = (0..8)
            .flat_map(|y| (0..8).map(move |x| (7 - y) * 8 + x))
            .map(|i| source[i])
            .collect();
        let preview: Vec<_> = raw.iter().map(|v| f16::from_f32(*v).to_f32()).collect();
        fs::write(
            project.join("science.f16"),
            preview
                .iter()
                .flat_map(|v| f16::from_f32(*v).to_bits().to_le_bytes())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let mut meta = json!({"width":8,"height":8,"wcs":header(8,8,1./3600.),
            "layers":[{"key":"science","file":"science.f16","source":"science.fits","wavelength_nm":550}],
            "stars":{"file":"stars.u16","detect":true,"count":1}});
        let metadata = meta.to_string();
        fs::write(project.join("meta.json"), &metadata).unwrap();
        let state = json!({"layers":[{"source":"science","mode":"tint","color":"#ffffff","stretch":"linear","blend":"add"}],"global":{}});
        // Design decision: abandoned star assets do not participate in calibration
        // or rendering. Keep the original project metadata on disk unchanged.
        let missing = render_export(session.path(), &state, Some(8), &|| false).unwrap();
        assert_eq!((missing.width, missing.height), (8, 8));
        assert!(missing.pixels.iter().all(|v| v.is_finite()));
        assert!(missing.pixels.iter().any(|v| *v > 0.));
        assert!(!project.join("stars.u16").exists());
        assert_eq!(
            fs::read_to_string(project.join("meta.json")).unwrap(),
            metadata
        );
        assert_eq!(fs::read_dir(&project).unwrap().count(), 2);
        meta.as_object_mut().unwrap().remove("stars");
        fs::write(project.join("meta.json"), meta.to_string()).unwrap();
        let absent = render_export(session.path(), &state, Some(8), &|| false).unwrap();
        assert_eq!(missing.pixels, absent.pixels);
        fs::write(project.join("meta.json"), &metadata).unwrap();

        // Present, missing, and malformed legacy labels all have the same result.
        let valid: Vec<_> = (0..64).map(|i| i >= 32).collect();
        let labels: Vec<_> = valid
            .iter()
            .flat_map(|v| u16::from(!*v).to_le_bytes())
            .collect();
        fs::write(project.join("stars.u16"), labels).unwrap();
        let expected = fit_tone(&raw, &preview, &[true; 64]).unwrap();
        let info = plan(session.path(), &state, Some(8)).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let prepared =
            prepare_project(session.path(), &state, &info, scratch.path(), &|| false).unwrap();
        let normalization = &prepared["layers"][0]["native_normalization"];
        assert_eq!(
            normalization["offset"].as_f64().unwrap().to_bits(),
            expected.0.to_bits()
        );
        assert_eq!(
            normalization["scale"].as_f64().unwrap().to_bits(),
            expected.1.to_bits()
        );

        fs::write(project.join("stars.u16"), [0_u8; 2]).unwrap();
        let malformed = render_export(session.path(), &state, Some(8), &|| false).unwrap();
        assert_eq!(missing.pixels, malformed.pixels);
        fs::remove_file(project.join("stars.u16")).unwrap();
        fs::create_dir(project.join("stars.u16")).unwrap();
        let directory = render_export(session.path(), &state, Some(8), &|| false).unwrap();
        assert_eq!(missing.pixels, directory.pixels);
    }

    #[test]
    fn merged_science_header_rotated_tan_wrap_and_top_down_reprojection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("science.fits");
        let hdr = header(12, 8, 1. / 3600.);
        let values: Vec<_> = (0..96).map(|v| v as f32 / 96.).collect();
        write_fits(&path, &values, &hdr, true);
        let source = Science::open(&path).unwrap();
        assert_eq!((source.image.width, source.image.height), (12, 8));
        let pixels = source.load(&|| false).unwrap();
        let target = Celestial::new(&hdr, 12, 8).unwrap();
        let projected = reproject(&pixels, &source.wcs, &target, 12, 8, &|| false).unwrap();
        for y in 0..8 {
            for x in 0..12 {
                assert!((projected[y * 12 + x] - values[(7 - y) * 12 + x]).abs() < 2e-6);
            }
        }
        let p = target.wcs.unproj(&fitsrs::ImgXY::new(12., 4.)).unwrap();
        let q = target.wcs.proj(&p).unwrap();
        assert!((q.x() - 12.).abs() < 1e-6 && (q.y() - 4.).abs() < 1e-6);
        assert!((target.scale - 1. / 3600.).abs() < 1e-12);
    }

    #[test]
    fn masked_gaps_are_missing_not_interpolated_black_and_fit_rejects_no_range() {
        let wcs = Celestial::new(&header(8, 8, 1. / 3600.), 8, 8).unwrap();
        let mut pixels = Raster {
            width: 8,
            height: 8,
            values: vec![1.; 64],
        };
        pixels.values[4 * 8 + 3] = f32::NAN;
        let result = reproject(&pixels, &wcs, &wcs, 8, 8, &|| false).unwrap();
        assert!(result[(7 - 4) * 8 + 3].is_nan());
        assert_eq!(result[0], 1.);
        let preview: Vec<_> = (0..64).map(|i| i as f32 / 64.).collect();
        let raw: Vec<_> = preview.iter().map(|p| 3. + 7. * p).collect();
        let (offset, scale) = fit_tone(&raw, &preview, &[true; 64]).unwrap();
        assert!((offset - 3.).abs() < 1e-6 && (scale - 7.).abs() < 1e-6);
        assert!(fit_tone(&[1.; 64], &[0.; 64], &[true; 64]).is_err());
    }

    #[test]
    fn survey_exports_use_available_fits_sampling_including_mixed_mosaics() {
        let library = tempfile::tempdir().unwrap();
        let session = library.path().join("sessions/surveys");
        let mut state = window_fixture(&session);
        state["layers"][0]["brush"]["strokes"]
            .as_array_mut()
            .unwrap()
            .extend([
                json!({"mode":"white","size":12,"hardness":0.3,"strength":0.7,"points":[[75,40]]}),
                json!({"mode":"black","size":8,"hardness":0.5,"strength":0.2,"points":[[82,44]]}),
            ]);
        let project = session.join("project");
        let original = read_json(&project.join("meta.json")).unwrap();
        for mosaic in [false, true] {
            let mut meta = original.clone();
            if mosaic {
                let mut tile = meta["layers"][1].clone();
                tile["gain"] = json!(1.);
                tile["offset"] = json!(0.);
                meta["layers"][1]["tiles"] = json!([tile]);
            }
            fs::write(project.join("meta.json"), meta.to_string()).unwrap();
            let expected_plan = plan(&session, &state, Some(43)).unwrap();
            let expected = render_export(&session, &state, Some(43), &|| false).unwrap();
            for (marker, id) in [
                ("hips_id", "CDS/P/DSS2/red"),
                ("hips_id", "CDS/P/allWISE/W3"),
                ("survey_id", "MDW/Halpha"),
                ("hips_id", "CDS/P/SPITZER/IRAC1"),
            ] {
                let mut survey = meta.clone();
                survey["layers"][0][marker] = json!(id);
                survey["layers"][0]["native_scale_arcsec"] = json!(0.001);
                fs::write(project.join("meta.json"), survey.to_string()).unwrap();
                let info = plan(&session, &state, Some(43)).unwrap();
                assert_eq!(info.native_factor, expected_plan.native_factor);
                assert_eq!(info.max_long_side, expected_plan.max_long_side);
                let rendered = render_export(&session, &state, Some(43), &|| false).unwrap();
                assert_eq!(
                    (rendered.width, rendered.height),
                    (expected.width, expected.height)
                );
                assert_eq!(rendered.pixels, expected.pixels, "{id}, mosaic={mosaic}");
            }
        }
        fs::remove_file(session.join("science.fits")).unwrap();
        assert!(plan(&session, &state, Some(43)).is_err());
        assert!(render_export(&session, &state, Some(43), &|| false).is_err());
    }

    #[test]
    fn flat_survey_exports_preserve_source_paint_without_a_tonal_fit() {
        let library = tempfile::tempdir().unwrap();
        let session = library.path().join("sessions/flat-survey");
        let project = session.join("project");
        fs::create_dir_all(&project).unwrap();
        let source = session.join("survey.fits");
        let wcs = header(32, 24, 2. / 3600.);
        let mut pixels = vec![42.; 32 * 24];
        pixels[11 * 32 + 16] = f32::NAN;
        write_fits(&source, &pixels, &wcs, false);
        let grid = json!({"width":32,"height":24,"wcs":wcs,"pixel_scale_arcsec":2.});
        let sources = vec![vec![BuildSource {
            path: source.clone(),
            observation: json!({"obs_id":"survey","hips_id":"CDS/P/allWISE/W3","filter":"W3"}),
        }]];
        prepare_build(
            &session,
            &project,
            &grid,
            &sources,
            false,
            0.,
            &crate::jobs::JobHandle::untracked(),
        )
        .unwrap();
        let meta = fs::read(project.join("meta.json")).unwrap();
        let source_before = fs::read(&source).unwrap();
        let state = json!({"layers":[{"source":"survey","mode":"tint","color":"#b9d1f3",
            "stretch":"linear","blend":"add","brush":{"strokes":[{
                "mode":"white","size":8,"hardness":0.2,"strength":0.6,"points":[[16.5,12.5]]
            }]}}],"global":{}});
        let expected = crate::render::render_project(&project, &state, None, &|| false).unwrap();
        let rendered = render_export(&session, &state, None, &|| false).unwrap();
        assert_eq!((rendered.width, rendered.height), (32, 24));
        assert!(rendered.pixels.iter().any(|value| *value > 0.1));
        assert_eq!(rendered.pixels, expected.pixels);
        assert_eq!(fs::read(project.join("meta.json")).unwrap(), meta);
        assert_eq!(fs::read(source).unwrap(), source_before);
    }

    #[test]
    fn unsupported_distortions_and_source_errors_never_become_native_success() {
        let mut h = header(8, 8, 1. / 3600.);
        h["CPDIS1"] = json!("LOOKUP");
        assert!(Celestial::new(&h, 8, 8).is_err());
        h.as_object_mut().unwrap().remove("CPDIS1");
        h["CTYPE1"] = json!("RA---TAN-SIP");
        assert!(Celestial::new(&h, 8, 8).is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("project")).unwrap();
        std::fs::write(
            dir.path().join("project/meta.json"),
            json!({
                "width":8,"height":8,"wcs":header(8,8,1./3600.),
                "layers":[{"key":"a","source":"missing.fits","file":"a.f16"}]
            })
            .to_string(),
        )
        .unwrap();
        let state = json!({"layers":[{"source":"a","mode":"tint"}],"global":{}});
        assert!(plan(dir.path(), &state, Some(8)).is_err());
        assert!(render_export(dir.path(), &state, None, &|| true).is_err());
    }

    #[test]
    fn weight_fringe_cut_keeps_thin_interior_seam_and_repairs_only_elongated_defect() {
        let (w, h) = (40, 40);
        let mut values: Vec<_> = (0..w * h).map(|p| 1. + p as f32 / 1600.).collect();
        let mut weight = vec![1_f32; w * h];
        for y in 8..32 {
            weight[y * w + 20] = 0.;
            values[y * w + 20] = -10.;
        }
        // Design decision: compact stellar cores are not repaired as chip seams.
        weight[10 * w + 10] = 0.;
        values[10 * w + 10] = 5.;
        let mut raster = Raster {
            width: w as u32,
            height: h as u32,
            values,
        };
        repair_streaks(&mut raster, &weight, &|| false).unwrap();
        assert!(raster.values[20 * w + 20] > 1.);
        assert_eq!(raster.values[10 * w + 10], 5.);
        cut_fringe(&mut raster, &weight, &|| false).unwrap();
        assert!(raster.values[0].is_nan());
        assert!(raster.values[20 * w + 20].is_finite());
        assert!(raster.values[10 * w + 10].is_finite());
        let mask = vec![true; 12];
        let d = distances(&mask, 4, 3, true, &|| false).unwrap();
        assert_eq!(d[0], 1.);
        assert_eq!(d[5], 2.);
    }

    #[test]
    fn public_plan_uses_visible_science_scale_not_finer_mask_but_luminance_requires_all() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("project")).unwrap();
        for (name, scale) in [("a", 1. / 3600.), ("b", 0.5 / 3600.)] {
            write_fits(
                &dir.path().join(format!("{name}.fits")),
                &vec![1.; 64],
                &header(8, 8, scale),
                false,
            );
        }
        let meta = json!({"width":8,"height":8,"wcs":header(8,8,2./3600.),
            "layers":[{"key":"a","file":"a.f16","source":"a.fits"},
                {"key":"b","file":"b.f16","source":"b.fits"}]});
        std::fs::write(dir.path().join("project/meta.json"), meta.to_string()).unwrap();
        let mut state =
            json!({"layers":[{"source":"a","mode":"tint","mask":{"source":"b"}}],"global":{}});
        let p = plan(dir.path(), &state, None).unwrap();
        assert!((p.native_factor - 2.).abs() < 1e-12);
        assert_eq!((p.width, p.height), (16, 16));
        assert_eq!(p.max_long_side, Some(16));
        assert_eq!(required(&meta, &state).unwrap().0.len(), 2);
        state["layers"][0]["source"] = json!("lum");
        assert!((plan(dir.path(), &state, None).unwrap().native_factor - 4.).abs() < 1e-12);
        state["layers"][0]["source"] = json!("a");
        state["layers"][0]["mode"] = json!("spectral");
        assert!((plan(dir.path(), &state, None).unwrap().native_factor - 4.).abs() < 1e-12);
        assert_eq!(plan(dir.path(), &state, Some(4)).unwrap().factor, 0.5);
        assert!(plan(dir.path(), &state, Some(33)).is_err());
        assert!(validate_work_budget(4096 * 3072, 4).is_ok());
        assert!(validate_work_budget(MAX_CANVAS_PIXELS + 1, 1).is_err());
        assert!(validate_work_budget(MAX_CANVAS_PIXELS, 5).is_err());
        // A header-only source larger than one allowed SCI allocation must not
        // make metadata-only GET planning fail or relabel cache as true native.
        // No declared SCI bytes exist: a pixel-loading plan would fail this test.
        write_fits(
            &dir.path().join("a.fits"),
            &[],
            &header((MAX_SOURCE_PIXELS + 1) as u32, 1, 1. / 3600.),
            false,
        );
        state["layers"][0]["mode"] = json!("tint");
        let p = plan(dir.path(), &state, None).unwrap();
        assert_eq!((p.width, p.height), (16, 16));
        assert_eq!(p.max_long_side, Some(0));
        let error = render_export(dir.path(), &state, None, &|| false)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("maximum supported long side is 0"));
    }

    #[test]
    fn eight_source_rotated_crop_admits_4k_with_actual_tile_bounds() {
        // Design decision: never hide user layers to satisfy aggregate admission.
        // The disk preparation may span the crop's rotated bounding rectangle,
        // but actual retained science and compositing allocations must be tiled.
        let session = tempfile::tempdir().unwrap();
        fs::create_dir(session.path().join("project")).unwrap();
        write_fits(
            &session.path().join("science.fits"),
            &[],
            &header(6144, 4096, 1. / 3600.),
            true,
        );
        let meta = json!({"width":3072,"height":2262,"wcs":header(3072,2262,3./3600.),
            "layers":(0..8).map(|i| json!({"key":format!("s{i}"),"file":"science.f16",
                "source":"science.fits"})).collect::<Vec<_>>()});
        fs::write(session.path().join("project/meta.json"), meta.to_string()).unwrap();
        let state = json!({"layers":(0..8).map(|i| json!({"source":format!("s{i}"),
            "edge":{"feather":47,"irregularity":0.76}})).collect::<Vec<_>>(),
            "global":{"rotation":-39.579242724962455,"crop":{"rotation":-39.579242724962455,
                "x":0.0943307933789268,"y":0.3245161981317972,
                "w":0.6041619913814611,"h":0.4615348841986144}}});
        let info = plan(session.path(), &state, Some(4096)).unwrap();
        assert_eq!((info.width, info.height), (4096, 2304));
        assert!(info.max_long_side.unwrap() >= 4096);
        let windows = preparation_windows(&meta, &state, info.factor).unwrap();
        assert!(validate_work_budget(windows.source.pixels().unwrap(), 8).is_err());
        assert!(validate_work_budget(windows.render.pixels().unwrap(), 0).is_err());
        windows.validate(8).unwrap();
    }

    #[test]
    #[ignore = "Set ASTROIMP_NATIVE_ORACLE to isolated parent Python fixture root"]
    fn python_export_oracle_native128_and_cached64() {
        let root = PathBuf::from(std::env::var("ASTROIMP_NATIVE_ORACLE").unwrap());
        let session = root.join("data/sessions/rust-validation");
        let state = read_json(&session.join("edits.json")).unwrap();
        let p = plan(&session, &state, None).unwrap();
        assert!((p.native_factor - 2.).abs() < 1e-10);
        assert_eq!((p.width, p.height), (128, 96));
        let scratch = tempfile::tempdir().unwrap();
        let references = read_json(&root.join("reference/render.json")).unwrap();
        for (side, edits, filename) in [
            (64, &state, "python-64.png"),
            (128, &state, "python-128.png"),
            (64, &references["tone"]["state"], "python-tone.png"),
        ] {
            let rgb = render_export(&session, edits, Some(side), &|| false).unwrap();
            let path = scratch.path().join(format!("{side}.png"));
            crate::render::write_image(&rgb, &path, "png8", 92).unwrap();
            let actual = image::open(path).unwrap().to_rgb8();
            let expected = image::open(root.join("reference").join(filename))
                .unwrap()
                .to_rgb8();
            assert_eq!(actual.dimensions(), expected.dimensions());
            let errors: Vec<_> = actual
                .as_raw()
                .iter()
                .zip(expected.as_raw())
                .map(|(a, b)| a.abs_diff(*b))
                .collect();
            let maximum = *errors.iter().max().unwrap();
            let mean = errors.iter().map(|v| *v as f64).sum::<f64>() / errors.len() as f64;
            println!("Python export {side} {filename}: max_u8={maximum}, mean_u8={mean:.8}");
            assert!(
                maximum <= 1,
                "Source-backed Python export mismatch: max={maximum}, mean={mean}"
            );
        }
    }

    #[test]
    #[ignore = "Set ASTROIMP_NATIVE_ORACLE and ASTROIMP_NATIVE_PNG16_ORACLE (Python export_full png16)"]
    fn native128_float32_png16_python_oracle_comparison() {
        let root = PathBuf::from(std::env::var("ASTROIMP_NATIVE_ORACLE").unwrap());
        let session = root.join("data/sessions/rust-validation");
        let state = read_json(&session.join("edits.json")).unwrap();
        let rendered = render_export(&session, &state, Some(128), &|| false).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("native128-png16.png");
        crate::render::write_image(&rendered, &path, "png16", 92).unwrap();
        let actual_image = image::open(path).unwrap();
        let expected_image =
            image::open(std::env::var("ASTROIMP_NATIVE_PNG16_ORACLE").unwrap()).unwrap();
        assert_eq!(actual_image.color(), image::ColorType::Rgb16);
        assert_eq!(expected_image.color(), image::ColorType::Rgb16);
        let actual = actual_image.to_rgb16();
        let expected = expected_image.to_rgb16();
        assert_eq!(actual.dimensions(), (128, 96));
        assert_eq!(actual.dimensions(), expected.dimensions());
        assert!(
            actual
                .as_raw()
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                > 256
        );
        let mut maximum = 0_u16;
        let mut sum = 0_f64;
        let mut squared = 0_f64;
        for (a, b) in actual.as_raw().iter().zip(expected.as_raw()) {
            let error = a.abs_diff(*b);
            maximum = maximum.max(error);
            sum += error as f64;
            squared += (error as f64).powi(2);
        }
        let n = actual.as_raw().len() as f64;
        // Diagnostic comparison against unchanged Python export_full, whose
        // intermediates are f16. Float32 preservation is independently asserted
        // by the bit-exact native_preparation test, not merely PNG header depth.
        println!(
            "Native128 PNG16 vs legacy Python f16-intermediate oracle: max_u16={maximum}, mean_abs_u16={:.9}, rmse_u16={:.9}",
            sum / n,
            (squared / n).sqrt()
        );
    }

    #[test]
    #[ignore = "Read-only format/WCS spike: set ASTROIMP_READONLY_REPO to fixture repository"]
    fn live_hst_jwst_science_headers_and_pixel_values() {
        let root = PathBuf::from(std::env::var("ASTROIMP_READONLY_REPO").unwrap());
        let cases = [
            (
                "NGC_6611/jw02739-o001_t001_nircam_clear-f187n_i2d.fits",
                14341,
                8583,
                8.67316085665305e-6,
                [274.6895343523669, -13.912795421384766],
                [274.6987425118664, -13.904413581404475],
            ),
            (
                "NGC_6611/jw02739-o002_t001_miri_f770w_i2d.fits",
                2305,
                2072,
                3.08073571014394e-5,
                [274.6890104838911, -13.874531526484855],
                [274.7244500306256, -13.847876050155039],
            ),
            (
                "HST/hst_13926_05_wfc3_uvis_f502n_ick905_drc.fits",
                6419,
                6555,
                1.1005555796954e-5,
                [274.7293860306637, -13.869712857088235],
                [274.7180601105591, -13.858719723539918],
            ),
            (
                "HST/hst_5773_05_wfpc2_pc_f502n_u2lx05_drz.fits",
                4677,
                4675,
                1.2646666666666e-5,
                [274.74116882673405, -13.860561568024416],
                [274.72815494864096, -13.847928815134985],
            ),
        ];
        for (name, w, h, scale, a, b) in cases {
            let source = Science::open(&root.join("data/downloads").join(name)).unwrap();
            assert_eq!((source.image.width, source.image.height), (w, h));
            assert!(source.weight.is_some());
            assert!((source.wcs.scale - scale).abs() < 1e-15);
            for (point, expected) in [(1., a), (1000., b)] {
                let sky = source
                    .wcs
                    .wcs
                    .unproj(&fitsrs::ImgXY::new(point, point))
                    .unwrap();
                let delta_lon =
                    (sky.lon().to_degrees() - expected[0] + 180.).rem_euclid(360.) - 180.;
                assert!(delta_lon.abs() < 1e-9);
                assert!((sky.lat().to_degrees() - expected[1]).abs() < 1e-9);
            }
            println!(
                "{name}: {}x{}, WHT, rotated TAN/CD/PC WCS parity <1e-9 degrees",
                w, h
            );
        }
        // Read the real MIRI full float32 image through the bounded loader; no
        // source preparation, writes, or giant NIRCam float32 allocations here.
        let source = Science::open(
            &root.join("data/downloads/NGC_6611/jw02739-o002_t001_miri_f770w_i2d.fits"),
        )
        .unwrap();
        let pixels = source.load(&|| false).unwrap();
        assert!(pixels.values[0].is_nan());
        assert!((pixels.values[999 * 2305 + 999] - 161.847_5).abs() < 1e-5);
    }

    #[test]
    #[ignore = "Set ASTROIMP_REAL_SESSION to private JWST fixture session; headers only"]
    fn real_four_layer_metadata_plan_reports_true_native_and_safe_4k_limit() {
        let session = PathBuf::from(std::env::var("ASTROIMP_REAL_SESSION").unwrap());
        let state = read_json(&session.join("edits.json")).unwrap();
        let p = plan(&session, &state, None).unwrap();
        assert!((p.native_factor - 4.755208380885425).abs() < 1e-12);
        assert_eq!((p.width, p.height), (9097, 14608));
        assert!(
            p.max_long_side
                .is_some_and(|side| (4096..14608).contains(&side))
        );
        println!(
            "Real headers-only plan: native={}x{}, factor={}, max_long_side={:?}",
            p.width, p.height, p.native_factor, p.max_long_side
        );
        let requested = plan(&session, &state, Some(4096)).unwrap();
        assert_eq!((requested.width, requested.height), (2551, 4096));
    }

    #[test]
    #[ignore = "Read-only/private real512 diagnostic; set ASTROIMP_REAL_SESSION"]
    fn real_four_layer_source_backed512_python_reference_difference_metrics() {
        let session = PathBuf::from(std::env::var("ASTROIMP_REAL_SESSION").unwrap());
        let state = read_json(&session.join("edits.json")).unwrap();
        let info = plan(&session, &state, Some(512)).unwrap();
        assert!(info.factor <= 1.);
        let rendered = render_export(&session, &state, Some(512), &|| false).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let output = scratch.path().join("rust-real512.png");
        crate::render::write_image(&rendered, &output, "png8", 92).unwrap();
        let actual = image::open(output).unwrap().to_rgb8();
        let mut references = vec![(
            "Python source-reprojected export",
            session.join("exports/python-real-reference-319x512.png"),
        )];
        if let Ok(path) = std::env::var("ASTROIMP_REAL_CACHED_REFERENCE") {
            references.push(("Python cached-plane renderer", PathBuf::from(path)));
        }
        for (label, path) in references {
            let expected = image::open(path).unwrap().to_rgb8();
            assert_eq!(actual.dimensions(), expected.dimensions());
            let mut maximum = 0_u8;
            let mut sum = 0_f64;
            let mut squared = 0_f64;
            for (a, b) in actual.as_raw().iter().zip(expected.as_raw()) {
                let error = a.abs_diff(*b);
                maximum = maximum.max(error);
                sum += error as f64;
                squared += (error as f64).powi(2);
            }
            let n = actual.as_raw().len() as f64;
            println!(
                "Real512 versus {label}: dimensions={}x{}, factor={}, max_u8={maximum}, mean_abs_u8={:.9}, rmse_u8={:.9}",
                actual.width(),
                actual.height(),
                info.factor,
                sum / n,
                (squared / n).sqrt()
            );
            if label == "Python source-reprojected export" {
                // Design decision: full export prepares sources at requested
                // resolution even below preview. Cached RGB downsizing belongs
                // only to the separate renderer/quick-export capability.
                assert!(
                    maximum <= 1,
                    "Real512 source-backed export drift: max={maximum}, mean={}, rmse={}",
                    sum / n,
                    (squared / n).sqrt()
                );
            }
        }
    }

    #[test]
    #[ignore = "Expensive read-only-source real4096 export; use release and private ASTROIMP_REAL_SESSION"]
    fn real_four_layer_4096_source_backed_export() {
        let session = PathBuf::from(std::env::var("ASTROIMP_REAL_SESSION").unwrap());
        let state = read_json(&session.join("edits.json")).unwrap();
        let p = plan(&session, &state, Some(4096)).unwrap();
        assert!(p.max_long_side.is_some_and(|side| side >= 4096));
        let started = std::time::Instant::now();
        let image = render_export(&session, &state, Some(4096), &|| false).unwrap();
        assert_eq!((image.width, image.height), (2551, 4096));
        assert!(image.pixels.iter().all(|v| v.is_finite()));
        assert!(image.pixels.iter().any(|v| *v > 0.));
        let scratch = tempfile::tempdir().unwrap();
        crate::render::write_image(&image, &scratch.path().join("native4096.png"), "png8", 92)
            .unwrap();
        println!(
            "Real4096 source-backed export {}x{}, wall_s={:.3}, private temporary PNG; originals untouched",
            image.width,
            image.height,
            started.elapsed().as_secs_f64()
        );
    }

    #[test]
    #[ignore = "Set ASTROIMP_COMPRESSION_FIXTURE to temp-generated Astropy FITS/oracle folder"]
    fn synthetic_compression_and_real_source_normalization_python_oracles() {
        let root = PathBuf::from(std::env::var("ASTROIMP_COMPRESSION_FIXTURE").unwrap());
        let cases = read_json(&root.join("cases.json")).unwrap();
        for case in cases.as_array().unwrap() {
            if case["file"].as_str() == Some("float-RICE_1.fits") {
                // User-approved restriction: this independently probed layout
                // overflowed in fitsrs. Reject it, do not change expected pixels
                // or let release wrapping arithmetic masquerade as verification.
                let error = Science::open(&root.join("float-RICE_1.fits"))
                    .err()
                    .unwrap()
                    .to_string();
                assert!(error.contains("Unsupported compressed floating"));
                println!(
                    "float-RICE_1.fits: rejected at header validation (verified library overflow)"
                );
                continue;
            }
            let source = Science::open(&root.join(case["file"].as_str().unwrap())).unwrap();
            let raster = source.load(&|| false).unwrap();
            let expected = case["values"].as_array().unwrap();
            let mut maximum = 0_f64;
            assert_eq!(raster.values.len(), expected.len());
            for (a, b) in raster.values.iter().zip(expected) {
                if b.is_null() {
                    assert!(a.is_nan());
                } else {
                    maximum = maximum.max((*a as f64 - b.as_f64().unwrap()).abs());
                }
            }
            println!("{} compressed/science max_abs={maximum}", case["file"]);
            assert!(maximum < 1e-5);
        }
        let source = Science::open(&root.join("real-miri-cutout.fits")).unwrap();
        let (pixels, wcs) = source.prepared(1, &|| false).unwrap();
        let target = wcs.scaled(1., pixels.width, pixels.height).unwrap();
        let raw = reproject(&pixels, &wcs, &target, pixels.width, pixels.height, &|| {
            false
        })
        .unwrap();
        let oracle = read_json(&root.join("real-normalization.json")).unwrap();
        let expected = oracle["prepared"].as_array().unwrap();
        let mut maximum = 0_f64;
        for (a, b) in raw.iter().zip(expected) {
            if b.is_null() {
                assert!(a.is_nan());
            } else {
                maximum = maximum.max((*a as f64 - b.as_f64().unwrap()).abs());
            }
        }
        println!("Real MIRI cutout preparation max_abs={maximum}");
        assert!(maximum < 1e-4);
        let preview: Vec<_> = oracle["preview"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap() as f32)
            .collect();
        let valid: Vec<_> = expected.iter().map(|v| !v.is_null()).collect();
        let (a, b) = fit_tone(&raw, &preview, &valid).unwrap();
        println!("Real MIRI tone fit offset={a:.9} scale={b:.9}");
        // Quantized build planes are the tone reference, not ideal unquantized
        // offset/scale; match Python's robust fit to those actual cached samples.
        assert!((a - oracle["offset"].as_f64().unwrap()).abs() < 1e-6);
        assert!((b - oracle["scale"].as_f64().unwrap()).abs() < 1e-6);
    }
}
