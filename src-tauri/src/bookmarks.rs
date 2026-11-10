use crate::projects;
use anyhow::{Context, Result, bail, ensure};
use chrono::Utc;
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf};
use uuid::Uuid;

pub struct Bookmarks {
    path: PathBuf,
}

impl Bookmarks {
    pub fn new(data: &std::path::Path) -> Self {
        Self {
            path: data.join("bookmarks.json"),
        }
    }

    pub fn list(&self) -> Result<Vec<Value>> {
        serde_json::from_value(projects::read_json(&self.path, json!([]))?)
            .context("Invalid bookmarks file: expected an array")
    }

    pub fn get(&self, identifier: &str) -> Result<Option<Value>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|item| item["id"].as_str() == Some(identifier)))
    }

    pub fn add(&self, rows: &[Value], session: &str, name: &str) -> Result<Vec<Value>> {
        let mut items = self.list()?;
        let mut known: HashSet<String> = items
            .iter()
            .filter_map(|item| item["obs_id"].as_str().map(str::to_owned))
            .collect();
        let before = items.len();
        for row in rows {
            ensure!(
                matches!(row["collection"].as_str(), Some("HST" | "JWST")),
                "Only HST and JWST images can be bookmarked."
            );
            let obs_id = row["obs_id"]
                .as_str()
                .filter(|value| !value.is_empty())
                .context("Observation has no ID")?;
            if !known.insert(obs_id.to_owned()) {
                continue;
            }
            let (center, extent) = bounds(std::slice::from_ref(row))?;
            let mut item = json!({});
            for key in [
                "obs_id",
                "collection",
                "instrument",
                "filter",
                "target",
                "wavelength_nm",
                "region",
            ] {
                item[key] = row[key].clone();
            }
            item["id"] = json!(Uuid::new_v4().simple().to_string());
            item["center_icrs_deg"] = json!(center);
            item["size_arcmin"] = json!(extent);
            item["preview"] = row["preview_uri"].clone();
            item["source_session"] = json!(session);
            item["source_name"] = json!(name);
            item["note"] = json!("");
            item["created"] = json!(Utc::now().to_rfc3339());
            items.push(item);
        }
        if items.len() != before {
            projects::write_json(&self.path, &json!(items))?;
        }
        Ok(items)
    }

    pub fn note(&self, identifier: &str, note: &str) -> Result<Option<Value>> {
        ensure!(
            note.chars().count() <= 200,
            "Notes can contain at most 200 characters."
        );
        let mut items = self.list()?;
        let Some(item) = items
            .iter_mut()
            .find(|item| item["id"].as_str() == Some(identifier))
        else {
            return Ok(None);
        };
        item["note"] = json!(note);
        let result = item.clone();
        projects::write_json(&self.path, &json!(items))?;
        Ok(Some(result))
    }

    pub fn remove(&self, identifier: &str) -> Result<bool> {
        let mut items = self.list()?;
        let before = items.len();
        items.retain(|item| item["id"].as_str() != Some(identifier));
        if items.len() == before {
            return Ok(false);
        }
        projects::write_json(&self.path, &json!(items))?;
        Ok(true)
    }
}

pub fn polygons(region: &str) -> Vec<Vec<[f64; 2]>> {
    region
        .to_ascii_uppercase()
        .split("POLYGON")
        .skip(1)
        .filter_map(|chunk| {
            let values = chunk
                .split_whitespace()
                .filter(|token| !token.chars().all(char::is_alphabetic))
                .map(str::parse::<f64>)
                .collect::<std::result::Result<Vec<_>, _>>()
                .ok()?;
            (values.len() >= 6 && values.len() % 2 == 0).then(|| values.as_chunks::<2>().0.to_vec())
        })
        .collect()
}

fn points(items: &[Value]) -> Result<Vec<[f64; 2]>> {
    let mut points = Vec::new();
    for item in items {
        let shapes = polygons(item["region"].as_str().unwrap_or(""));
        if shapes.is_empty() {
            let center = item.get("center_icrs_deg");
            let ra = center.map_or(&item["ra"], |value| &value[0]);
            let dec = center.map_or(&item["dec"], |value| &value[1]);
            points.push([
                ra.as_f64()
                    .context("This observation has no usable sky footprint.")?,
                dec.as_f64()
                    .context("This observation has no usable sky footprint.")?,
            ]);
        } else {
            points.extend(shapes.into_iter().flatten());
        }
    }
    ensure!(!points.is_empty(), "Select at least one bookmark.");
    ensure!(
        points
            .iter()
            .all(|p| p[0].is_finite() && p[1].is_finite() && p[1].abs() <= 90.),
        "This observation has no usable sky footprint."
    );
    Ok(points)
}

pub fn aspect_ratio(aspect: &str) -> Result<f64> {
    match aspect {
        "16:9" => Ok(16. / 9.),
        "3:2" => Ok(1.5),
        "1:1" => Ok(1.),
        "9:16" => Ok(9. / 16.),
        _ => bail!("Unknown aspect ratio."),
    }
}

fn project(center: [f64; 2], sky: [f64; 2]) -> Result<[f64; 2]> {
    let [ra0, dec0] = center.map(f64::to_radians);
    let [ra, dec] = sky.map(f64::to_radians);
    let delta = ra - ra0;
    let denominator = dec0.sin() * dec.sin() + dec0.cos() * dec.cos() * delta.cos();
    ensure!(
        denominator > 0.,
        "These footprints are too far apart. Narrow the selection."
    );
    Ok([
        (-dec.cos() * delta.sin() / denominator).to_degrees() * 60.,
        ((dec0.cos() * dec.sin() - dec0.sin() * dec.cos() * delta.cos()) / denominator)
            .to_degrees()
            * 60.,
    ])
}

fn unproject(center: [f64; 2], pixel: [f64; 2]) -> [f64; 2] {
    let [ra0, dec0] = center.map(f64::to_radians);
    let x = (-pixel[0] / 60.).to_radians();
    let y = (pixel[1] / 60.).to_radians();
    let denominator = dec0.cos() - y * dec0.sin();
    [
        (ra0 + x.atan2(denominator)).to_degrees().rem_euclid(360.),
        (dec0.sin() + y * dec0.cos())
            .atan2(denominator.hypot(x))
            .to_degrees(),
    ]
}

fn bounds(items: &[Value]) -> Result<([f64; 2], [f64; 2])> {
    let points = points(items)?;
    let mut vector = [0.; 3];
    for [ra, dec] in &points {
        let (ra, dec) = (ra.to_radians(), dec.to_radians());
        vector[0] += dec.cos() * ra.cos() / points.len() as f64;
        vector[1] += dec.cos() * ra.sin() / points.len() as f64;
        vector[2] += dec.sin() / points.len() as f64;
    }
    ensure!(
        vector[0].hypot(vector[1]).hypot(vector[2]) >= 1e-8,
        "These footprints are too far apart. Narrow the selection."
    );
    let center = [
        vector[1].atan2(vector[0]).to_degrees().rem_euclid(360.),
        vector[2].atan2(vector[0].hypot(vector[1])).to_degrees(),
    ];
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for &point in &points {
        let pixel = project(center, point)?;
        for axis in 0..2 {
            lo[axis] = lo[axis].min(pixel[axis]);
            hi[axis] = hi[axis].max(pixel[axis]);
        }
    }
    let center = unproject(center, [(lo[0] + hi[0]) / 2., (lo[1] + hi[1]) / 2.]);
    let mut extent = [0_f64; 2];
    for point in points {
        let pixel = project(center, point)?;
        for axis in 0..2 {
            extent[axis] = extent[axis].max(2. * pixel[axis].abs());
        }
    }
    Ok((center, extent))
}

pub fn frame(items: &[Value], aspect: &str) -> Result<Value> {
    let ratio = aspect_ratio(aspect)?;
    let (center, extent) = bounds(items)?;
    let required = extent[0].max(extent[1] * ratio) * 1.1;
    let limit = 30. * ratio.min(1.);
    let width = required.max(ratio.max(1.)).min(limit);
    let exceeds = required > limit;
    let round = |value: f64| (value * 1000.).round() / 1000.;
    Ok(json!({
        "ra":center[0],"dec":center[1],"aspect":aspect,
        "width_arcmin":round(width),"height_arcmin":round(width/ratio),
        "required_width_arcmin":round(required),"exceeds_limit":exceeds,
        "warning":exceeds.then_some("Footprints exceed 30′. This frame is cropped; narrow the selection to include every image.")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(id: &str, ra: f64, width: f64, height: f64) -> Value {
        let corners = [
            [-width / 2., -height / 2.],
            [width / 2., -height / 2.],
            [width / 2., height / 2.],
            [-width / 2., height / 2.],
        ];
        let region = corners
            .map(|point| {
                let [ra, dec] = unproject([ra, 20.], point);
                format!("{ra} {dec}")
            })
            .join(" ");
        json!({"obs_id":id,"collection":"HST","instrument":"WFC3","filter":"F656N",
            "target":"Field","wavelength_nm":656,"ra":ra,"dec":20,
            "region":format!("POLYGON ICRS {region}"),"preview_uri":"mast:HST/product/preview.jpg"})
    }

    #[test]
    fn bookmarks_preserve_imported_data_and_failed_batches_are_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let store = Bookmarks::new(dir.path());
        let row = observation("one", 12., 4., 2.);
        let items = store
            .add(&[row.clone(), row.clone()], "original", "Field")
            .unwrap();
        assert_eq!(items.len(), 1);
        let identifier = items[0]["id"].as_str().unwrap();
        assert_eq!(store.add(&[row], "different", "Other").unwrap(), items);
        store.note(identifier, "Hydrogen edge").unwrap();
        let reopened = Bookmarks::new(dir.path());
        assert_eq!(
            reopened.get(identifier).unwrap().unwrap()["note"],
            "Hydrogen edge"
        );
        let before = std::fs::read(&store.path).unwrap();
        let mut survey = observation("survey", 12., 4., 2.);
        survey["collection"] = json!("Survey");
        assert!(
            store
                .add(&[observation("new", 12., 4., 2.), survey], "s", "s")
                .is_err()
        );
        assert!(store.note(identifier, &"x".repeat(201)).is_err());
        assert_eq!(std::fs::read(&store.path).unwrap(), before);
        assert!(reopened.remove(identifier).unwrap());
        assert!(!store.remove(identifier).unwrap());
        assert!(store.list().unwrap().is_empty());
        std::fs::write(&store.path, b"broken json").unwrap();
        assert!(store.list().is_err());
    }

    #[test]
    fn footprint_frames_match_astropy_padding_wrap_and_limits() {
        let single = frame(&[observation("one", 12., 4., 2.)], "16:9").unwrap();
        assert!((single["ra"].as_f64().unwrap() - 12.).abs() < 1e-5);
        assert!((single["dec"].as_f64().unwrap() - 20.).abs() < 1e-5);
        assert_eq!(single["width_arcmin"], 4.4);
        for (left, right, expected_ra) in [(12., 12.1, 12.05), (359.95, 0.05, 0.)] {
            let result = frame(
                &[
                    observation("a", left, 2., 2.),
                    observation("b", right, 2., 2.),
                ],
                "16:9",
            )
            .unwrap();
            let ra = result["ra"].as_f64().unwrap();
            let distance = (ra - expected_ra + 180.).rem_euclid(360.) - 180.;
            assert!(distance.abs() < 1e-5, "{result}");
            assert_eq!(result["exceeds_limit"], false);
        }
        for aspect in ["16:9", "1:1", "9:16"] {
            let result = frame(&[observation("tiny", 12., 0.01, 0.01)], aspect).unwrap();
            assert!(result["width_arcmin"].as_f64().unwrap() >= 1.);
            assert!(result["height_arcmin"].as_f64().unwrap() >= 1.);
        }
        let wide = frame(
            &[observation("a", 12., 4., 2.), observation("b", 13., 4., 2.)],
            "16:9",
        )
        .unwrap();
        assert_eq!(wide["width_arcmin"], 30.);
        assert_eq!(wide["exceeds_limit"], true);
        assert!(wide["required_width_arcmin"].as_f64().unwrap() > 60.);
        assert!(frame(&[], "16:9").is_err());
        assert!(frame(&[json!({"ra":12,"dec":100})], "16:9").is_err());
    }
}
