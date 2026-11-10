use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use fitsrs::{Fits, HDU, WCS, WCSParams};
use serde_json::{Value, json};
use std::io::Cursor;

use crate::native::{frame_bias, header_json, linear_matrix};

pub fn fits_header(text: &str) -> Result<Value> {
    ensure!(text.is_ascii(), "FITS/WCS metadata must be ASCII.");
    let mut bytes = Vec::new();
    if text.contains('\n') {
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            ensure!(line.len() <= 80, "Invalid FITS/WCS card length.");
            bytes.extend_from_slice(line.as_bytes());
            bytes.resize(bytes.len() + 80 - line.len(), b' ');
        }
    } else {
        ensure!(
            text.len().is_multiple_of(80),
            "Invalid FITS/WCS card length."
        );
        bytes.extend_from_slice(text.as_bytes());
    }
    if !bytes.chunks(80).any(|card| card.starts_with(b"END     ")) {
        bytes.extend_from_slice(
            b"END                                                                             ",
        );
    }
    bytes.resize(bytes.len().div_ceil(2880) * 2880, b' ');
    let mut fits = Fits::from_reader(Cursor::new(bytes));
    match fits.next().context("Missing FITS/WCS header")?? {
        HDU::Primary(hdu) | HDU::XImage(hdu) => Ok(header_json(hdu.get_header())),
        _ => bail!("Snapshot WCS is not an image header."),
    }
}

pub struct SkyWcs {
    wcs: WCS,
    tangent: Option<Tangent>,
    pub header: Value,
    fk5: bool,
    matrix: [f64; 4],
    reference: [f64; 2],
}

struct Tangent {
    ra: f64,
    sin_dec: f64,
    cos_dec: f64,
    sin_pole: f64,
    cos_pole: f64,
}

impl Tangent {
    fn unproj(&self, plane: [f64; 2]) -> fitsrs::LonLat {
        let [x, y] = plane.map(f64::to_radians);
        let (x, y) = (
            self.cos_pole * x - self.sin_pole * y,
            self.sin_pole * x + self.cos_pole * y,
        );
        let denominator = self.cos_dec - y * self.sin_dec;
        fitsrs::LonLat::new(
            (self.ra + x.atan2(denominator)).rem_euclid(std::f64::consts::TAU),
            (self.sin_dec + y * self.cos_dec).atan2(x.hypot(denominator)),
        )
    }

    fn proj(&self, sky: &fitsrs::LonLat) -> Option<fitsrs::ImgXY> {
        let (sin_ra, cos_ra) = (sky.lon() - self.ra).sin_cos();
        let (sin_dec, cos_dec) = sky.lat().sin_cos();
        let denominator = self.sin_dec * sin_dec + self.cos_dec * cos_dec * cos_ra;
        if denominator <= 0. {
            return None;
        }
        let x = cos_dec * sin_ra / denominator;
        let y = (self.cos_dec * sin_dec - self.sin_dec * cos_dec * cos_ra) / denominator;
        Some(fitsrs::ImgXY::new(
            (self.cos_pole * x + self.sin_pole * y).to_degrees(),
            (-self.sin_pole * x + self.cos_pole * y).to_degrees(),
        ))
    }
}

impl SkyWcs {
    pub fn new(header: &Value, width: u32, height: u32) -> Result<Self> {
        ensure!(width > 0 && height > 0, "Invalid sky frame dimensions.");
        let header = if let Some(text) = header["fits_header"].as_str() {
            fits_header(text)?
        } else {
            header.clone()
        };
        let mut h = header.as_object().context("Missing sky WCS")?.clone();
        let ctype1 = h.get("CTYPE1").and_then(Value::as_str).unwrap_or("");
        let ctype2 = h.get("CTYPE2").and_then(Value::as_str).unwrap_or("");
        ensure!(
            ctype1.starts_with("RA---")
                && ctype2.starts_with("DEC--")
                && ctype1.len() == 8
                && ctype2.len() == 8
                && ctype1[5..] == ctype2[5..],
            "Capture requires an undistorted equatorial RA/Dec WCS."
        );
        for key in h.keys() {
            ensure!(
                ![
                    "CPDIS", "DP1", "DP2", "D2IMDIS", "DET2IM", "A_", "B_", "AP_", "BP_"
                ]
                .iter()
                .any(|prefix| key.starts_with(prefix)),
                "Unsupported snapshot distortion keyword {key}"
            );
        }
        for key in ["CUNIT1", "CUNIT2"] {
            ensure!(
                h.get(key).and_then(Value::as_str).unwrap_or("deg").trim() == "deg",
                "Unsupported sky WCS units in {key}"
            );
        }
        for key in ["CRPIX1", "CRPIX2", "CRVAL1", "CRVAL2"] {
            ensure!(
                h.get(key)
                    .and_then(Value::as_f64)
                    .is_some_and(f64::is_finite),
                "Missing/non-finite sky WCS {key}"
            );
        }
        ensure!(
            (-90. ..=90.).contains(&h["CRVAL2"].as_f64().unwrap()),
            "Invalid reference declination."
        );
        let tangent = if h["CTYPE1"] == "RA---TAN" {
            for (key, default) in [("PV1_0", 0.), ("PV1_1", 0.), ("PV1_2", 90.)] {
                ensure!(
                    h.get(key)
                        .is_none_or(|value| value.as_f64() == Some(default)),
                    "Unsupported TAN fiducial keyword {key}"
                );
            }
            let dec = h["CRVAL2"].as_f64().unwrap();
            let pole =
                h.get("LONPOLE")
                    .map_or(Ok(if dec >= 90. { 0. } else { 180. }), |value| {
                        value
                            .as_f64()
                            .filter(|value| value.is_finite())
                            .context("Invalid TAN pole longitude")
                    })?;
            let (sin_dec, cos_dec) = dec.to_radians().sin_cos();
            let (sin_pole, cos_pole) = (180. - pole).to_radians().sin_cos();
            Some(Tangent {
                ra: h["CRVAL1"].as_f64().unwrap().to_radians(),
                sin_dec,
                cos_dec,
                sin_pole,
                cos_pole,
            })
        } else {
            None
        };
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
            matches!(frame, "FK5" | "ICRS"),
            "Unsupported snapshot frame {frame}"
        );
        let fk5 = frame == "FK5";
        ensure!(
            !fk5 || (h.get("EQUINOX").and_then(Value::as_f64).unwrap_or(2000.) - 2000.).abs()
                < 1e-10,
            "Only FK5 J2000 snapshots are supported."
        );
        let matrix = linear_matrix(&Value::Object(h.clone()))?;
        ensure!(
            matrix.iter().all(|v| v.is_finite())
                && (matrix[0] * matrix[3] - matrix[1] * matrix[2]).abs() > 1e-30,
            "Singular sky WCS pixel matrix"
        );
        for key in [
            "PC1_1", "PC1_2", "PC2_1", "PC2_2", "CDELT1", "CDELT2", "CROTA1", "CROTA2",
        ] {
            h.remove(key);
        }
        for (key, value) in ["CD1_1", "CD1_2", "CD2_1", "CD2_2"].into_iter().zip(matrix) {
            h.insert(key.into(), json!(value));
        }
        h.insert("NAXIS".into(), json!(2));
        h.insert("NAXIS1".into(), json!(width));
        h.insert("NAXIS2".into(), json!(height));
        h.insert("RADESYS".into(), json!("ICRS"));
        h.remove("RADECSYS");
        h.remove("EQUINOX");
        let reference = [h["CRPIX1"].as_f64().unwrap(), h["CRPIX2"].as_f64().unwrap()];
        let mut projection = h.clone();
        // Keep the affine map explicit: fitsrs' inverse CD path is not a general
        // matrix inverse for skewed pixels. Its celestial projection is independent.
        for (key, value) in [
            ("CRPIX1", 0.),
            ("CRPIX2", 0.),
            ("CD1_1", 1.),
            ("CD1_2", 0.),
            ("CD2_1", 0.),
            ("CD2_2", 1.),
        ] {
            projection.insert(key.into(), json!(value));
        }
        let params: WCSParams = serde_json::from_value(Value::Object(projection))?;
        let wcs = WCS::new(&params)
            .map_err(|error| anyhow::anyhow!("Invalid sky projection: {error:?}"))?;
        h.insert("RADESYS".into(), json!(if fk5 { "FK5" } else { "ICRS" }));
        if fk5 {
            h.insert("EQUINOX".into(), json!(2000.));
        }
        Ok(Self {
            wcs,
            tangent,
            header: Value::Object(h),
            fk5,
            matrix,
            reference,
        })
    }

    /// Zero-based FITS (bottom-up) pixel centres to ICRS degrees.
    pub fn pixel_to_sky(&self, point: [f64; 2]) -> Option<[f64; 2]> {
        let sky = self.unproj(&fitsrs::ImgXY::new(point[0] + 1., point[1] + 1.))?;
        let result = [
            sky.lon().to_degrees().rem_euclid(360.),
            sky.lat().to_degrees(),
        ];
        result.iter().all(|v| v.is_finite()).then_some(result)
    }

    pub fn sky_to_pixel(&self, point: [f64; 2]) -> Option<[f64; 2]> {
        let sky = fitsrs::LonLat::new(point[0].to_radians(), point[1].to_radians());
        let pixel = self.proj(&sky)?;
        let result = [pixel.x() - 1., pixel.y() - 1.];
        result.iter().all(|v| v.is_finite()).then_some(result)
    }

    pub(crate) fn unproj(&self, pixel: &fitsrs::ImgXY) -> Option<fitsrs::LonLat> {
        let (x, y) = (pixel.x() - self.reference[0], pixel.y() - self.reference[1]);
        let plane = [
            self.matrix[0] * x + self.matrix[1] * y,
            self.matrix[2] * x + self.matrix[3] * y,
        ];
        if !plane.iter().all(|value| value.is_finite()) {
            return None;
        }
        // Analytic TAN avoids mapproj's tight unit-vector assertion after rotation.
        let mut sky = if let Some(tangent) = &self.tangent {
            tangent.unproj(plane)
        } else {
            self.wcs.unproj(&fitsrs::ImgXY::new(plane[0], plane[1]))?
        };
        if self.fk5 {
            sky = frame_bias(sky, false);
        }
        Some(sky)
    }

    pub(crate) fn proj(&self, sky: &fitsrs::LonLat) -> Option<fitsrs::ImgXY> {
        let mut sky = sky.clone();
        if self.fk5 {
            sky = frame_bias(sky, true);
        }
        let pixel = if let Some(tangent) = &self.tangent {
            tangent.proj(&sky)?
        } else {
            self.wcs.proj(&sky)?
        };
        let [a, b, c, d] = self.matrix;
        let determinant = a * d - b * c;
        Some(fitsrs::ImgXY::new(
            (d * pixel.x() - b * pixel.y()) / determinant + self.reference[0],
            (a * pixel.y() - c * pixel.x()) / determinant + self.reference[1],
        ))
    }

    pub fn rotated(&self, angle: f64) -> Result<Self> {
        ensure!(
            angle.is_finite() && (-180. ..=180.).contains(&angle),
            "Rotation must be between -180 and 180 degrees."
        );
        let mut header = self.header.clone();
        if angle != 0. {
            let [a, b, d, e] = linear_matrix(&header)?;
            let (s, c) = angle.to_radians().sin_cos();
            let matrix = [c * a + s * d, c * b + s * e, -s * a + c * d, -s * b + c * e];
            let center = [
                (header["NAXIS1"].as_f64().unwrap() + 1.) / 2.,
                (header["NAXIS2"].as_f64().unwrap() + 1.) / 2.,
            ];
            let dx = header["CRPIX1"].as_f64().unwrap() - center[0];
            let dy = header["CRPIX2"].as_f64().unwrap() - center[1];
            let old = [a * dx + b * dy, d * dx + e * dy];
            let determinant = matrix[0] * matrix[3] - matrix[1] * matrix[2];
            header["CRPIX1"] =
                json!(center[0] + (matrix[3] * old[0] - matrix[1] * old[1]) / determinant);
            header["CRPIX2"] =
                json!(center[1] + (matrix[0] * old[1] - matrix[2] * old[0]) / determinant);
            for (key, value) in ["CD1_1", "CD1_2", "CD2_1", "CD2_2"].into_iter().zip(matrix) {
                header[key] = json!(value);
            }
        }
        Self::new(
            &header,
            header["NAXIS1"].as_u64().unwrap() as u32,
            header["NAXIS2"].as_u64().unwrap() as u32,
        )
    }
}

pub fn separation(a: [f64; 2], b: [f64; 2]) -> f64 {
    let (a1, d1, a2, d2) = (
        a[0].to_radians(),
        a[1].to_radians(),
        b[0].to_radians(),
        b[1].to_radians(),
    );
    let da = a2 - a1;
    (d2.cos() * da.sin())
        .hypot(d1.cos() * d2.sin() - d1.sin() * d2.cos() * da.cos())
        .atan2(d1.sin() * d2.sin() + d1.cos() * d2.cos() * da.cos())
        .to_degrees()
}

fn geometry(wcs: &SkyWcs, width: u32, height: u32) -> Result<Value> {
    let (w, h) = (width as f64, height as f64);
    let (cx, cy) = ((w - 1.) / 2., (h - 1.) / 2.);
    let points = [
        [cx, cy],
        [-0.5, cy],
        [w - 0.5, cy],
        [cx, -0.5],
        [cx, h - 0.5],
        [-0.5, -0.5],
        [w - 0.5, -0.5],
        [w - 0.5, h - 0.5],
        [-0.5, h - 0.5],
    ]
    .into_iter()
    .map(|point| {
        wcs.pixel_to_sky(point)
            .context("Snapshot extends beyond its sky projection.")
    })
    .collect::<Result<Vec<_>>>()?;
    let field = [
        separation(points[1], points[2]) * 60.,
        separation(points[3], points[4]) * 60.,
    ];
    ensure!(
        field.iter().all(|value| value.is_finite() && *value > 0.),
        "Invalid snapshot angular dimensions."
    );
    Ok(json!({
        "center_icrs_deg":points[0],"field_arcmin":field,"corners_icrs_deg":points[5..],
        "pixel_dimensions":[width,height],
        "projection":[wcs.header["CTYPE1"],wcs.header["CTYPE2"]]
    }))
}

pub struct Capture {
    pub view: Value,
    pub original: Value,
    pub header: Value,
    pub jpeg: Option<Vec<u8>>,
}

impl Capture {
    pub fn wcs(&self) -> Result<SkyWcs> {
        let (width, height) = dimensions(&self.original)?;
        SkyWcs::new(&self.header, width, height)?
            .rotated(self.view["rotation_deg"].as_f64().unwrap_or(0.))
    }
}

fn dimensions(view: &Value) -> Result<(u32, u32)> {
    let dim = |index: usize| -> Result<u32> {
        let value = view["pixel_dimensions"][index]
            .as_u64()
            .context("Missing capture dimensions")?;
        ensure!(
            value > 0 && value <= u32::MAX as u64,
            "Invalid capture dimensions"
        );
        Ok(value as u32)
    };
    Ok((dim(0)?, dim(1)?))
}

pub fn rotated_view(original: &Value, header: &Value, angle: f64) -> Result<Value> {
    let (width, height) = dimensions(original)?;
    let wcs = SkyWcs::new(header, width, height)?.rotated(angle)?;
    let mut view = original.clone();
    view["rotation_deg"] = json!(angle);
    if angle != 0. {
        view.as_object_mut()
            .context("Invalid original capture")?
            .extend(geometry(&wcs, width, height)?.as_object().unwrap().clone());
    }
    Ok(view)
}

pub fn parse_snapshot(value: &str) -> Result<Capture> {
    let encoded = value
        .strip_prefix("data:image/jpeg;base64,")
        .context("Aladin returned an unexpected snapshot format.")?;
    let encoded: Vec<u8> = encoded
        .bytes()
        .filter(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
        .collect();
    let jpeg = STANDARD
        .decode(encoded)
        .context("Aladin returned invalid snapshot data.")?;
    ensure!(
        jpeg.starts_with(b"\xff\xd8"),
        "Aladin snapshot is not a JPEG."
    );
    let mut position = 2;
    let mut header = None;
    let mut dimensions = None;
    while position < jpeg.len() {
        ensure!(jpeg[position] == 255, "Invalid JPEG marker");
        while position < jpeg.len() && jpeg[position] == 255 {
            position += 1;
        }
        let marker = *jpeg.get(position).context("Truncated JPEG marker")?;
        position += 1;
        if matches!(marker, 0xda | 0xd9) {
            break;
        }
        if marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
            continue;
        }
        let bytes = jpeg
            .get(position..position + 2)
            .context("Truncated JPEG segment length")?;
        let length = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        ensure!(length >= 2, "Invalid JPEG segment length");
        let payload = jpeg
            .get(position + 2..position + length)
            .context("Truncated JPEG segment")?;
        if marker == 0xfe && payload.starts_with(b"SIMPLE") {
            let text = std::str::from_utf8(payload).context("Invalid Aladin WCS header")?;
            let parsed = fits_header(text)?;
            header = Some(parsed);
        }
        if matches!(marker, 0xc0..=0xc2) {
            ensure!(payload.len() >= 6, "Truncated JPEG dimensions");
            dimensions = Some((
                u16::from_be_bytes([payload[3], payload[4]]) as u32,
                u16::from_be_bytes([payload[1], payload[2]]) as u32,
            ));
        }
        position += length;
    }
    let header = header.context("Snapshot lacks FITS/WCS metadata.")?;
    let (width, height) = dimensions.context("Snapshot lacks pixel dimensions.")?;
    let wcs = SkyWcs::new(&header, width, height)?;
    let mut view = geometry(&wcs, width, height)?;
    view["header_pixel_dimensions"] = json!([header["NAXIS1"], header["NAXIS2"]]);
    view["snapshot_bytes"] = json!(jpeg.len());
    view["source"] = json!("Aladin snapshot.get.jpg embedded FITS/WCS");
    view["captured_at_utc"] = json!(chrono::Utc::now().to_rfc3339());
    view["rotation_deg"] = json!(0.);
    Ok(Capture {
        original: view.clone(),
        view,
        header,
        jpeg: Some(jpeg),
    })
}

pub fn coordinate_capture(request: &Value) -> Result<Capture> {
    let ra = request["ra"]
        .as_f64()
        .context("Supply a finite right ascension.")?;
    let dec = request["dec"]
        .as_f64()
        .context("Supply a finite declination.")?;
    let number = |key: &str, default| {
        request.get(key).map_or(Ok(default), |value| {
            value
                .as_f64()
                .with_context(|| format!("{key} must be a number."))
        })
    };
    let width_arcmin = number("width_arcmin", 10.)?;
    let rotation = number("rotation_deg", 0.)?;
    ensure!(
        ra.is_finite() && (0. ..360.).contains(&ra),
        "RA must be between 0 and 360 degrees."
    );
    ensure!(
        dec.is_finite() && (-90. ..=90.).contains(&dec),
        "Dec must be between -90 and 90 degrees."
    );
    ensure!(
        width_arcmin.is_finite() && (1. ..=180.).contains(&width_arcmin),
        "Width must be between 1 and 180 arcminutes."
    );
    let aspect = request.get("aspect").map_or(Ok("16:9"), |value| {
        value.as_str().context("Aspect must be a frame ratio.")
    })?;
    let aspect: f64 = match aspect {
        "16:9" => 16. / 9.,
        "3:2" => 3. / 2.,
        "1:1" => 1.,
        "9:16" => 9. / 16.,
        _ => bail!("Unsupported frame aspect."),
    };
    let width = (1600. * aspect.min(1.)).round() as u32;
    let height = (1600. / aspect.max(1.)).round() as u32;
    let scale = 2. * (width_arcmin / 120.).to_radians().tan().to_degrees() / width as f64;
    let header = json!({
        "SIMPLE":true,"BITPIX":8,"NAXIS":2,"NAXIS1":width,"NAXIS2":height,
        "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CUNIT1":"deg","CUNIT2":"deg",
        "CRVAL1":ra,"CRVAL2":dec,"CRPIX1":(width as f64+1.)/2.,"CRPIX2":(height as f64+1.)/2.,
        "CDELT1":-scale,"CDELT2":scale,"RADESYS":"ICRS"
    });
    let wcs = SkyWcs::new(&header, width, height)?;
    let mut original = geometry(&wcs, width, height)?;
    original["header_pixel_dimensions"] = json!([width, height]);
    original["snapshot_bytes"] = json!(0);
    original["source"] = json!("coordinates");
    original["captured_at_utc"] = json!(chrono::Utc::now().to_rfc3339());
    original["rotation_deg"] = json!(0.);
    original["surveys_only"] = json!(request.get("surveys_only").map_or(Ok(false), |value| {
        value
            .as_bool()
            .context("surveys_only must be true or false.")
    })?);
    let view = rotated_view(&original, &header, rotation)?;
    Ok(Capture {
        original,
        view,
        header,
        jpeg: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projections_and_rotations_match_astropy() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../tests/fixtures/sky.json")).unwrap();
        for case in fixtures.as_array().unwrap() {
            let width = case["width"].as_u64().unwrap() as u32;
            let height = case["height"].as_u64().unwrap() as u32;
            let wcs = SkyWcs::new(&case["header"], width, height)
                .unwrap()
                .rotated(case["angle"].as_f64().unwrap())
                .unwrap();
            for sample in case["samples"].as_array().unwrap() {
                let point = [
                    sample["pixel"][0].as_f64().unwrap(),
                    sample["pixel"][1].as_f64().unwrap(),
                ];
                let expected = [
                    sample["sky"][0].as_f64().unwrap(),
                    sample["sky"][1].as_f64().unwrap(),
                ];
                let actual = wcs.pixel_to_sky(point).unwrap();
                assert!(
                    separation(actual, expected) < 1e-8,
                    "{} angle {} pixel {point:?}: {actual:?} != {expected:?}",
                    case["header"]["CTYPE1"],
                    case["angle"]
                );
                let recovered = wcs.sky_to_pixel(expected).unwrap();
                assert!(
                    (recovered[0] - point[0]).abs() < 1e-5
                        && (recovered[1] - point[1]).abs() < 1e-5,
                    "{} {} angle {}: {recovered:?} != {point:?}",
                    case["header"]["CTYPE1"],
                    case["header"]["RADESYS"],
                    case["angle"]
                );
            }
        }
    }

    #[test]
    fn coordinate_frames_keep_center_dimensions_and_lossless_reset() {
        for aspect in ["16:9", "3:2", "1:1", "9:16"] {
            let capture = coordinate_capture(&json!({"ra":359.99,"dec":72.,"width_arcmin":17.,"aspect":aspect,"rotation_deg":53.})).unwrap();
            let center = [
                capture.view["center_icrs_deg"][0].as_f64().unwrap(),
                capture.view["center_icrs_deg"][1].as_f64().unwrap(),
            ];
            assert!(separation(center, [359.99, 72.]) < 1e-8);
            let reset = rotated_view(&capture.original, &capture.header, 0.).unwrap();
            assert_eq!(reset, capture.original);
            assert!((capture.original["field_arcmin"][0].as_f64().unwrap() - 17.).abs() < 1e-8);
        }
        assert!(coordinate_capture(&json!({"ra":0,"dec":91})).is_err());
        assert!(coordinate_capture(&json!({"ra":0,"dec":0,"width_arcmin":0})).is_err());
        for (key, value) in [
            ("width_arcmin", json!("10")),
            ("rotation_deg", Value::Null),
            ("aspect", json!(1)),
            ("surveys_only", json!(1)),
        ] {
            let mut request = json!({"ra":0,"dec":0});
            request[key] = value;
            assert!(coordinate_capture(&request).is_err(), "{key}");
        }
    }

    #[test]
    fn jpeg_uses_actual_dimensions_and_reports_malformed_metadata() {
        let cards = [
            "SIMPLE  =                    T",
            "BITPIX  =                    8",
            "NAXIS   =                    2",
            "NAXIS1  =                   11",
            "NAXIS2  =                    7",
            "CTYPE1  = 'RA---AIT'",
            "CTYPE2  = 'DEC--AIT'",
            "CRPIX1  =                  6.0",
            "CRPIX2  =                  4.0",
            "CRVAL1  =                 83.0",
            "CRVAL2  =                 -5.0",
            "CDELT1  =               -0.001",
            "CDELT2  =                0.001",
            "END",
        ]
        .join("\n");
        let mut image = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(10, 6)
            .write_to(&mut image, image::ImageFormat::Jpeg)
            .unwrap();
        let original = image.into_inner();
        let mut jpeg = vec![255, 216, 255, 254];
        jpeg.extend_from_slice(&((cards.len() + 2) as u16).to_be_bytes());
        jpeg.extend_from_slice(cards.as_bytes());
        jpeg.extend_from_slice(&original[2..]);
        let parsed = parse_snapshot(&format!(
            "data:image/jpeg;base64,{}",
            STANDARD.encode(&jpeg)
        ))
        .unwrap();
        assert_eq!(parsed.view["pixel_dimensions"], json!([10, 6]));
        assert_eq!(parsed.view["header_pixel_dimensions"], json!([11, 7]));
        assert_eq!(parsed.view["projection"], json!(["RA---AIT", "DEC--AIT"]));
        assert!(parse_snapshot("data:image/jpeg;base64,%%%").is_err());
        assert!(
            parse_snapshot(&format!(
                "data:image/jpeg;base64,{}",
                STANDARD.encode(original)
            ))
            .is_err()
        );
    }
}
