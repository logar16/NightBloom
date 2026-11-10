//! Source astrometry for independent survey FITS. Snapshot WCS intentionally
//! rejects distortion; science cutouts additionally need TAN-SIP and DSS plates.
use super::*;

pub(super) enum SourceWcs {
    Plain(SkyWcs),
    Sip {
        base: SkyWcs,
        reference: [f64; 2],
        a: Vec<(i32, i32, f64)>,
        b: Vec<(i32, i32, f64)>,
    },
    Dss {
        tangent: SkyWcs,
        a: [f64; 13],
        b: [f64; 13],
        offset: [f64; 2],
        pixel_um: [f64; 2],
        origin_um: [f64; 2],
        scale: f64,
    },
}

impl SourceWcs {
    pub fn new(header: &Value, width: u32, height: u32) -> Result<Self> {
        if header.get("AMDX1").is_some() && header.get("AMDY1").is_some() {
            let required = |key: &str| {
                numeric(header, key)
                    .ok_or_else(|| anyhow::anyhow!("The DSS plate has incomplete astrometry."))
            };
            let ra =
                (required("PLTRAH")? + required("PLTRAM")? / 60. + required("PLTRAS")? / 3600.)
                    * 15.;
            let dec =
                (required("PLTDECD")? + required("PLTDECM")? / 60. + required("PLTDECS")? / 3600.)
                    * if text(header, "PLTDECSN") == "-" {
                        -1.
                    } else {
                        1.
                    };
            ensure!(
                (numeric(header, "PLTEQUIN")
                    .or_else(|| numeric(header, "EQUINOX"))
                    .unwrap_or(2000.)
                    - 2000.)
                    .abs()
                    < 1e-8,
                "This DSS plate uses unsupported non-J2000 astrometry."
            );
            let tangent = SkyWcs::new(
                &json!({
                    "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CRVAL1":ra,"CRVAL2":dec,
                    "CRPIX1":1.0,"CRPIX2":1.0,"CDELT1":1./3600.,"CDELT2":1./3600.,
                    "RADESYS":"FK5","EQUINOX":2000.0
                }),
                1,
                1,
            )?;
            let mut a = [0.; 13];
            let mut b = [0.; 13];
            for index in 0..13 {
                a[index] = required(&format!("AMDX{}", index + 1))?;
                b[index] = required(&format!("AMDY{}", index + 1))?;
            }
            let scale = required("PLTSCALE")?;
            let pixel_um = [required("XPIXELSZ")?, required("YPIXELSZ")?];
            ensure!(
                scale > 0. && pixel_um.iter().all(|v| *v > 0.),
                "Invalid DSS plate scale."
            );
            return Ok(Self::Dss {
                tangent,
                a,
                b,
                offset: [required("CNPIX1")? + 0.5, required("CNPIX2")? + 0.5],
                pixel_um,
                origin_um: [required("PPO3")?, required("PPO6")?],
                scale,
            });
        }
        let has_sip = text(header, "CTYPE1").ends_with("-SIP")
            || header.get("A_ORDER").is_some()
            || header.get("B_ORDER").is_some();
        if !has_sip {
            return Ok(Self::Plain(SkyWcs::new(header, width, height)?));
        }
        let mut normalized = header.clone();
        for key in ["CTYPE1", "CTYPE2"] {
            normalized[key] = json!(
                text(header, key)
                    .strip_suffix("-SIP")
                    .unwrap_or(text(header, key))
            );
        }
        let polynomial = |prefix: &str| -> Result<Vec<(i32, i32, f64)>> {
            let order = header[format!("{prefix}_ORDER")].as_u64().unwrap_or(0);
            ensure!(
                (2..=9).contains(&order),
                "Unsupported survey SIP polynomial order."
            );
            let mut terms = Vec::new();
            for i in 0..=order {
                for j in 0..=order - i {
                    if let Some(value) = numeric(header, &format!("{prefix}_{i}_{j}"))
                        && value != 0.
                    {
                        terms.push((i as i32, j as i32, value));
                    }
                }
            }
            Ok(terms)
        };
        let a = polynomial("A")?;
        let b = polynomial("B")?;
        normalized.as_object_mut().unwrap().retain(|key, _| {
            !["A_", "B_", "AP_", "BP_"]
                .iter()
                .any(|prefix| key.starts_with(prefix))
        });
        let reference = [
            numeric(header, "CRPIX1").unwrap_or(0.) - 1.,
            numeric(header, "CRPIX2").unwrap_or(0.) - 1.,
        ];
        Ok(Self::Sip {
            base: SkyWcs::new(&normalized, width, height)?,
            reference,
            a,
            b,
        })
    }

    pub fn pixel_to_sky(&self, point: [f64; 2]) -> Option<[f64; 2]> {
        match self {
            Self::Plain(wcs) => wcs.pixel_to_sky(point),
            Self::Sip {
                base,
                reference,
                a,
                b,
            } => {
                let p = [point[0] - reference[0], point[1] - reference[1]];
                base.pixel_to_sky([point[0] + sip(a, p), point[1] + sip(b, p)])
            }
            Self::Dss {
                tangent,
                a,
                b,
                offset,
                pixel_um,
                origin_um,
                ..
            } => {
                let x = (origin_um[0] - (point[0] + offset[0]) * pixel_um[0]) / 1000.;
                let y = ((point[1] + offset[1]) * pixel_um[1] - origin_um[1]) / 1000.;
                tangent.pixel_to_sky([plate(a, x, y), plate(b, y, x)])
            }
        }
    }

    pub fn sky_to_pixel(&self, point: [f64; 2]) -> Option<[f64; 2]> {
        match self {
            Self::Plain(wcs) => wcs.sky_to_pixel(point),
            Self::Sip {
                base,
                reference,
                a,
                b,
            } => {
                let target = base.sky_to_pixel(point)?;
                let desired = [target[0] - reference[0], target[1] - reference[1]];
                let forward = |p: [f64; 2]| [p[0] + sip(a, p), p[1] + sip(b, p)];
                let p = inverse(forward, desired, desired, 1e-7)?;
                Some([p[0] + reference[0], p[1] + reference[1]])
            }
            Self::Dss {
                tangent,
                a,
                b,
                offset,
                pixel_um,
                origin_um,
                scale,
            } => {
                let desired = tangent.sky_to_pixel(point)?;
                let p = inverse(
                    |p| [plate(a, p[0], p[1]), plate(b, p[1], p[0])],
                    desired,
                    [desired[0] / scale, desired[1] / scale],
                    1e-6,
                )?;
                Some([
                    (origin_um[0] - 1000. * p[0]) / pixel_um[0] - offset[0],
                    (origin_um[1] + 1000. * p[1]) / pixel_um[1] - offset[1],
                ])
            }
        }
    }
}

fn sip(terms: &[(i32, i32, f64)], p: [f64; 2]) -> f64 {
    terms
        .iter()
        .map(|(i, j, c)| c * p[0].powi(*i) * p[1].powi(*j))
        .sum()
}

fn plate(a: &[f64; 13], x: f64, y: f64) -> f64 {
    let r2 = x * x + y * y;
    a[0] * x
        + a[1] * y
        + a[2]
        + a[3] * x * x
        + a[4] * x * y
        + a[5] * y * y
        + a[6] * r2
        + a[7] * x * x * x
        + a[8] * x * x * y
        + a[9] * x * y * y
        + a[10] * y * y * y
        + a[11] * x * r2
        + a[12] * x * r2 * r2
}

fn inverse(
    forward: impl Fn([f64; 2]) -> [f64; 2],
    desired: [f64; 2],
    mut p: [f64; 2],
    tolerance: f64,
) -> Option<[f64; 2]> {
    for _ in 0..25 {
        let v = forward(p);
        let error = [desired[0] - v[0], desired[1] - v[1]];
        if error[0].hypot(error[1]) < tolerance {
            return Some(p);
        }
        let delta = 1e-3;
        let dx = forward([p[0] + delta, p[1]]);
        let dy = forward([p[0], p[1] + delta]);
        let (a, b, c, d) = (
            (dx[0] - v[0]) / delta,
            (dy[0] - v[0]) / delta,
            (dx[1] - v[1]) / delta,
            (dy[1] - v[1]) / delta,
        );
        let determinant = a * d - b * c;
        if !determinant.is_finite() || determinant.abs() < 1e-20 {
            return None;
        }
        p[0] += (d * error[0] - b * error[1]) / determinant;
        p[1] += (a * error[1] - c * error[0]) / determinant;
        if !p.iter().all(|n| n.is_finite()) {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dss_plate_matches_offline_astropy_icrs_oracle() {
        // Design decision: use the actual DSS plate polynomial, not a local
        // linear approximation that drifts over a photographic plate.
        // Values and ICRS points from Astropy's installed DSS WCS test dataset.
        let h = json!({
            "CNPIX1":8860,"CNPIX2":1708,"XPIXELSZ":25.28445,"YPIXELSZ":25.28445,
            "PPO3":176616.9833815,"PPO6":177193.56115606,"PLTRAH":14,"PLTRAM":37,"PLTRAS":46.88253,
            "PLTDECSN":"-","PLTDECD":60,"PLTDECM":12,"PLTDECS":59.28761,"PLTSCALE":67.2,
            "AMDX1":67.226158492105,"AMDX2":-0.23024540155842,"AMDX3":1.0901493794544,
            "AMDX4":1.1835555135256e-6,"AMDX5":5.5613287069447e-6,"AMDX6":-3.8419945268889e-5,
            "AMDX7":0.0,"AMDX8":2.2770722559048e-6,"AMDX9":1.0335160125448e-7,
            "AMDX10":2.3321693523829e-6,"AMDX11":1.8565977703272e-7,"AMDX12":0.0,"AMDX13":0.0,
            "AMDY1":67.23331978069,"AMDY2":0.22606735154764,"AMDY3":-0.52719954923422,
            "AMDY4":-7.3037555803828e-6,"AMDY5":1.9544740562734e-5,"AMDY6":-5.6277730545338e-6,
            "AMDY7":0.0,"AMDY8":2.2086510085905e-6,"AMDY9":1.0292330494988e-8,
            "AMDY10":2.1738798605975e-6,"AMDY11":-1.5811855244111e-8,"AMDY12":0.0,"AMDY13":0.0
        });
        let source = SourceWcs::new(&h, 100, 100).unwrap();
        for (sky, expected) in [
            ([217.53320542378066, -62.709141274441954], [0., 0.]),
            ([217.5131718718996, -62.69945581871064], [20., 20.]),
            ([217.4831458145796, -62.68492238407596], [50., 50.]),
            ([217.43416580938236, -62.661170914082426], [99., 99.]),
        ] {
            let actual = source.sky_to_pixel(sky).unwrap();
            assert!(
                (actual[0] - expected[0]).hypot(actual[1] - expected[1]) < 1e-5,
                "{actual:?} != {expected:?}"
            );
            assert!(separation(source.pixel_to_sky(expected).unwrap(), sky) < 1e-8);
        }
    }

    #[test]
    fn sip_is_inverted_not_silently_removed() {
        let mut header = json!({
            "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CRPIX1":50.5,"CRPIX2":50.5,
            "CRVAL1":10.,"CRVAL2":20.,"CDELT1":-0.001,"CDELT2":0.001
        });
        let base = SkyWcs::new(&header, 100, 100).unwrap();
        header["CTYPE1"] = json!("RA---TAN-SIP");
        header["CTYPE2"] = json!("DEC--TAN-SIP");
        header["A_ORDER"] = json!(2);
        header["B_ORDER"] = json!(2);
        header["A_2_0"] = json!(0.001);
        header["B_0_2"] = json!(-0.002);
        let source = SourceWcs::new(&header, 100, 100).unwrap();
        let pixel = [80., 20.];
        let uv = [pixel[0] - 49.5, pixel[1] - 49.5];
        let sky = base
            .pixel_to_sky([
                pixel[0] + 0.001 * uv[0] * uv[0],
                pixel[1] - 0.002 * uv[1] * uv[1],
            ])
            .unwrap();
        let actual = source.sky_to_pixel(sky).unwrap();
        assert!((actual[0] - pixel[0]).hypot(actual[1] - pixel[1]) < 1e-6);
        assert!(separation(source.pixel_to_sky(pixel).unwrap(), sky) < 1e-10);
    }
}
