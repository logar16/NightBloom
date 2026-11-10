use super::*;
use crate::{jobs::JobHandle, projects};
use sha2::{Digest, Sha256};

#[path = "building/timing.rs"]
mod timing;
use timing::{Task, Timing};

pub const BUILD_VERSION: &str = "nightbloom-native-build-v1";

#[derive(Clone)]
pub struct BuildSource {
    pub observation: Value,
    pub path: PathBuf,
}

pub fn source_metadata(path: &Path) -> Result<Value> {
    let science = Science::open(path)?;
    Ok(json!({
        "width":science.image.width,"height":science.image.height,
        "wcs":science.wcs.header,"pixel_scale_arcsec":science.wcs.scale*3600.
    }))
}

pub fn layer_key(observations: &[Value]) -> Result<String> {
    let first = observations.first().context("Empty build group")?;
    let value = if observations.len() == 1 {
        first["obs_id"]
            .as_str()
            .context("Missing observation ID")?
            .to_owned()
    } else {
        let identity = [
            "collection",
            "instrument",
            "filter",
            "proposal",
            "proposal_id",
        ]
        .map(|key| first.get(key).cloned().unwrap_or(Value::Null));
        format!(
            "mosaic_{}_{:x}",
            first["filter"].as_str().unwrap_or(""),
            Sha256::digest(serde_json::to_vec(&identity)?)
        )
    };
    let key: String = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || "_.-".contains(character) {
                character
            } else {
                '_'
            }
        })
        .collect();
    ensure!(
        !key.is_empty() && key != "." && key != "..",
        "Unsafe observation ID"
    );
    Ok(key)
}

fn reference(session: &Path, path: &Path) -> Result<String> {
    let library = session
        .parent()
        .and_then(Path::parent)
        .context("Project is not in a library")?
        .canonicalize()?;
    let path = path.canonicalize()?;
    let relative = path
        .strip_prefix(library)
        .context("Science source is outside the library")?;
    Ok(format!(
        "library:{}",
        relative.to_str().context("Science path is not Unicode")?
    ))
}

fn normalized(image: &mut [f32], cancelled: &dyn Fn() -> bool) -> Result<(Value, f64)> {
    let valid_count = image.iter().filter(|value| value.is_finite()).count();
    let coverage = valid_count as f64 / image.len() as f64;
    let sample: Vec<f64> = image
        .iter()
        .step_by((image.len() / 1_000_000).max(1))
        .filter(|value| value.is_finite())
        .map(|value| *value as f64)
        .collect();
    if sample.is_empty() {
        image.fill(0.);
        return Ok((
            json!({"p01":0.,"p50":0.,"p99":0.,"p999":0.,"max":0.,"background":0.,"scale":1.}),
            coverage,
        ));
    }
    let mut clipped = sample.clone();
    for _ in 0..5 {
        let median = percentile(&clipped, 0.5);
        let mean = clipped.iter().sum::<f64>() / clipped.len() as f64;
        let sigma = (clipped
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / clipped.len() as f64)
            .sqrt();
        let next: Vec<_> = clipped
            .iter()
            .copied()
            .filter(|value| (value - median).abs() <= 3. * sigma)
            .collect();
        if next.len() == clipped.len() || next.is_empty() {
            break;
        }
        clipped = next;
    }
    let background = percentile(&clipped, 0.5);
    let positive: Vec<_> = sample
        .into_iter()
        .filter(|value| *value > background)
        .map(|value| (value as f32 - background as f32) as f64)
        .collect();
    let scale = if positive.is_empty() {
        1.
    } else {
        percentile(&positive, 0.995)
    };
    let scale = if scale > 0. { scale } else { 1. };
    let mut finite = Vec::with_capacity(valid_count);
    for (index, value) in image.iter_mut().enumerate() {
        if index % 16384 == 0 {
            check(cancelled)?;
        }
        if value.is_finite() {
            *value = ((*value - background as f32) / scale as f32).clamp(-65504., 65504.);
            finite.push(*value);
        } else {
            *value = 0.;
        }
    }
    finite.sort_unstable_by(f32::total_cmp);
    let quantile = |q: f64| {
        let p = (finite.len() - 1) as f64 * q;
        let lo = p.floor() as usize;
        let hi = p.ceil() as usize;
        finite[lo] as f64 * (1. - (p - lo as f64)) + finite[hi] as f64 * (p - lo as f64)
    };
    Ok((
        json!({
            "p01":quantile(0.01),"p50":quantile(0.5),
            "p99":quantile(0.99),"p999":quantile(0.999),
            "max":finite.last().unwrap(),
            "background":background,"scale":scale
        }),
        coverage,
    ))
}

fn write_half(path: &Path, values: &[f32], cancelled: &dyn Fn() -> bool) -> Result<Vec<f32>> {
    let mut file = BufWriter::new(File::create(path)?);
    let mut quantized = Vec::with_capacity(values.len());
    for (index, value) in values.iter().enumerate() {
        if index % 16384 == 0 {
            check(cancelled)?;
        }
        let value = f16::from_f32(*value);
        file.write_all(&value.to_bits().to_le_bytes())?;
        quantized.push(value.to_f32());
    }
    file.flush()?;
    Ok(quantized)
}

pub(crate) fn samples(
    image: &[f32],
    width: u32,
    height: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let (w, h) = (width as usize, height as usize);
    let stride = w.max(h).div_ceil(768);
    let mut result = Vec::new();
    for y in (0..h).step_by(stride) {
        check(cancelled)?;
        for x in (0..w).step_by(stride) {
            let valid = x >= 4
                && y >= 4
                && x + 4 < w
                && y + 4 < h
                && (-4_isize..=4).all(|dy| {
                    let radius = 4 - dy.abs();
                    (-radius..=radius).all(|dx| {
                        image[(y as isize + dy) as usize * w + (x as isize + dx) as usize]
                            .is_finite()
                    })
                });
            if !valid {
                result.push(f32::NAN);
                continue;
            }
            let mut neighborhood = [0_f32; 25];
            let mut index = 0;
            for dy in -2_isize..=2 {
                for dx in -2_isize..=2 {
                    neighborhood[index] =
                        image[(y as isize + dy) as usize * w + (x as isize + dx) as usize];
                    index += 1;
                }
            }
            neighborhood.select_nth_unstable_by(12, f32::total_cmp);
            result.push(neighborhood[12]);
        }
    }
    Ok(result)
}

pub(crate) fn pair_fit(a: &[f32], b: &[f32]) -> Option<(f64, f64, usize)> {
    let values: Vec<_> = a
        .iter()
        .zip(b)
        .filter(|(a, b)| a.is_finite() && b.is_finite())
        .map(|(a, b)| (*a as f64, *b as f64))
        .collect();
    if values.len() < 64 {
        return None;
    }
    let a: Vec<_> = values.iter().map(|p| p.0).collect();
    let b: Vec<_> = values.iter().map(|p| p.1).collect();
    let (lo, hi, low, high) = (
        percentile(&a, 0.05),
        percentile(&a, 0.95),
        percentile(&b, 0.05),
        percentile(&b, 0.95),
    );
    let mut keep: Vec<_> = values
        .iter()
        .map(|(a, b)| *a > lo && *a < hi && *b > low && *b < high)
        .collect();
    let (mut gain, mut offset) = (1., 0.);
    for _ in 0..5 {
        let chosen: Vec<_> = values
            .iter()
            .zip(&keep)
            .filter(|(_, keep)| **keep)
            .map(|(value, _)| *value)
            .collect();
        if chosen.len() < 32 {
            return None;
        }
        let n = chosen.len() as f64;
        let x = chosen.iter().map(|p| p.1).sum::<f64>() / n;
        let y = chosen.iter().map(|p| p.0).sum::<f64>() / n;
        let variance = chosen.iter().map(|p| (p.1 - x).powi(2)).sum::<f64>() / n;
        if variance.sqrt() < 1e-12_f64.max(x.abs() * 1e-7) {
            return None;
        }
        gain = chosen.iter().map(|p| (p.1 - x) * (p.0 - y)).sum::<f64>() / n / variance;
        offset = percentile(
            &chosen.iter().map(|p| p.0 - gain * p.1).collect::<Vec<_>>(),
            0.5,
        );
        let residual: Vec<_> = values.iter().map(|p| p.0 - (gain * p.1 + offset)).collect();
        let chosen: Vec<_> = residual
            .iter()
            .zip(&keep)
            .filter(|(_, keep)| **keep)
            .map(|(value, _)| *value)
            .collect();
        let median = percentile(&chosen, 0.5);
        let scatter = 1.4826
            * percentile(
                &chosen
                    .iter()
                    .map(|value| (value - median).abs())
                    .collect::<Vec<_>>(),
                0.5,
            );
        for (keep, residual) in keep.iter_mut().zip(residual) {
            *keep &= residual.abs() <= (4. * scatter).max(1e-10);
        }
    }
    (gain.is_finite() && gain > 0. && offset.is_finite()).then_some((
        gain,
        offset,
        keep.iter().filter(|value| **value).count(),
    ))
}

fn solve(mut matrix: Vec<Vec<f64>>, mut values: Vec<f64>) -> Result<Vec<f64>> {
    let n = values.len();
    for column in 0..n {
        let pivot = (column..n)
            .max_by(|a, b| {
                matrix[*a][column]
                    .abs()
                    .total_cmp(&matrix[*b][column].abs())
            })
            .unwrap();
        ensure!(
            matrix[pivot][column].abs() > 1e-12,
            "Mosaic calibration is singular."
        );
        matrix.swap(column, pivot);
        values.swap(column, pivot);
        let (pivots, following) = matrix.split_at_mut(column + 1);
        let pivot_row = &pivots[column];
        for (offset, row) in following.iter_mut().enumerate() {
            let ratio = row[column] / pivot_row[column];
            for (entry, pivot) in row[column..].iter_mut().zip(&pivot_row[column..]) {
                *entry -= ratio * pivot;
            }
            values[column + 1 + offset] -= ratio * values[column];
        }
    }
    let mut result = vec![0.; n];
    for row in (0..n).rev() {
        result[row] = (values[row]
            - (row + 1..n)
                .map(|column| matrix[row][column] * result[column])
                .sum::<f64>())
            / matrix[row][row];
    }
    Ok(result)
}

fn calibrate(
    samples: &[Vec<f32>],
    coverage: &[usize],
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<Value>> {
    let count = samples.len();
    let reference = (0..count).fold(0, |best, index| {
        if coverage[index] > coverage[best] {
            index
        } else {
            best
        }
    });
    let mut edges = Vec::new();
    for i in 0..count {
        check(cancelled)?;
        for j in i + 1..count {
            if let Some((gain, offset, weight)) = pair_fit(&samples[i], &samples[j]) {
                edges.push((i, j, gain, offset, weight as f64));
            }
        }
    }
    let (mut gains, mut offsets, mut connected) =
        (vec![1.; count], vec![0.; count], vec![false; count]);
    let mut remaining = vec![true; count];
    while remaining.iter().any(|value| *value) {
        let root = if remaining[reference] {
            reference
        } else {
            (0..count)
                .filter(|i| remaining[*i])
                .reduce(|best, index| {
                    if coverage[index] > coverage[best] {
                        index
                    } else {
                        best
                    }
                })
                .unwrap()
        };
        let mut component = vec![false; count];
        component[root] = true;
        loop {
            let previous = component.clone();
            for (i, j, ..) in &edges {
                if component[*i] || component[*j] {
                    component[*i] = true;
                    component[*j] = true;
                }
            }
            if previous == component {
                break;
            }
        }
        for (remaining, included) in remaining.iter_mut().zip(&component) {
            *remaining &= !included;
        }
        let variables: Vec<_> = (0..count)
            .filter(|index| component[*index] && *index != root)
            .collect();
        if variables.is_empty() {
            continue;
        }
        for (connected, included) in connected.iter_mut().zip(&component) {
            *connected |= included;
        }
        let n = variables.len();
        let mut normal = vec![vec![0.; n]; n];
        let mut log_values = vec![0.; n];
        let rows: Vec<_> = edges
            .iter()
            .filter(|(i, ..)| component[*i])
            .map(|(i, j, gain, offset, weight)| {
                let row: Vec<_> = variables
                    .iter()
                    .map(|index| {
                        if index == i {
                            -1.
                        } else if index == j {
                            1.
                        } else {
                            0.
                        }
                    })
                    .collect();
                (row, *i, *gain, *offset, *weight)
            })
            .collect();
        for (row, _, gain, _, weight) in &rows {
            for a in 0..n {
                log_values[a] += row[a] * gain.ln() * weight;
                for b in 0..n {
                    normal[a][b] += row[a] * row[b] * weight;
                }
            }
        }
        for (index, value) in variables.iter().zip(solve(normal.clone(), log_values)?) {
            gains[*index] = value.exp();
        }
        let mut offset_values = vec![0.; n];
        for (row, i, _, offset, weight) in rows {
            for a in 0..n {
                offset_values[a] += row[a] * gains[i] * offset * weight;
            }
        }
        for (index, value) in variables.iter().zip(solve(normal, offset_values)?) {
            offsets[*index] = value;
        }
    }
    Ok((0..count)
        .map(|index| {
            json!({
                "gain":gains[index],"offset":offsets[index],
                "reference":index==reference,"no_overlap":!connected[index]
            })
        })
        .collect())
}

fn aligned(
    path: &Path,
    target: &Celestial,
    width: u32,
    height: u32,
    cancelled: &dyn Fn() -> bool,
    mut timing: Option<&mut Timing<'_>>,
) -> Result<Vec<f32>> {
    if let Some(timing) = timing.as_deref_mut() {
        timing.start("Preparing source pixels")?;
    }
    let source = Science::open(path)?;
    let block = (target.scale / source.wcs.scale + 1e-8).floor().max(1.) as u32;
    let (pixels, wcs) = source.prepared(block, cancelled)?;
    if let Some(timing) = timing.as_deref_mut() {
        timing.finish()?;
        timing.start("Reprojecting source pixels")?;
    }
    let image = if let Some(timing) = timing.as_deref_mut() {
        reproject_observed(
            (&pixels, &wcs),
            target,
            height,
            window::Region {
                x: 0,
                y: 0,
                width,
                height,
            },
            cancelled,
            &mut |rows| timing.advance(rows as f64 * width as f64),
        )?
    } else {
        reproject(&pixels, &wcs, target, width, height, cancelled)?
    };
    if let Some(timing) = timing {
        timing.finish()?;
    }
    Ok(image)
}

fn sort_work(pixels: usize) -> f64 {
    let pixels = pixels.max(1) as f64;
    pixels * pixels.max(2.).log2()
}

fn alignment_tasks(
    groups: &[Vec<BuildSource>],
    target: &Celestial,
    width: u32,
    height: u32,
) -> Result<Vec<Task>> {
    let pixels = width as f64 * height as f64;
    let mut tasks = Vec::new();
    for (step, group) in groups.iter().enumerate() {
        ensure!(!group.is_empty(), "Empty alignment source group");
        for source in group {
            let science = Science::open(&source.path)?;
            let block = (target.scale / science.wcs.scale + 1e-8).floor().max(1.) as u32;
            tasks.push(Task::new(
                format!(
                    "prepare:{}:{}:{}:{}",
                    science.image.header.get("ZCMPTYPE").unwrap_or(&Value::Null),
                    science
                        .image
                        .header
                        .get("ZBITPIX")
                        .unwrap_or(&science.image.header["BITPIX"]),
                    science.weight.is_some(),
                    block > 1
                ),
                science.image.width as f64 * science.image.height as f64,
                step,
            ));
            tasks.push(Task::new(
                format!(
                    "warp:{}:{}:{}",
                    science.wcs.header["CTYPE1"],
                    target.header["CTYPE1"],
                    science.wcs.fk5 != target.fk5
                ),
                pixels,
                step,
            ));
            if group.len() > 1 {
                tasks.push(Task::new("mosaic-spool", pixels, step));
            }
        }
        if group.len() > 1 {
            let stride = width.max(height).div_ceil(768);
            let samples = width.div_ceil(stride) as f64 * height.div_ceil(stride) as f64;
            let pairs = group.len() * (group.len() - 1) / 2;
            tasks.push(Task::new("mosaic-calibrate", samples * pairs as f64, step));
            for _ in group {
                tasks.push(Task::new("mosaic-blend", pixels, step));
            }
            tasks.push(Task::new("mosaic-combine", pixels, step));
        }
        tasks.push(Task::new("normalize", sort_work(pixels as usize), step));
        tasks.push(Task::new("write-layer", pixels, step));
    }
    let mut finish = Task::new("write-luminance", pixels, groups.len());
    finish.phase = "publish";
    tasks.push(finish);
    Ok(tasks)
}

fn mosaic(
    sources: &[BuildSource],
    session: &Path,
    folder: &Path,
    target: &Celestial,
    width: u32,
    height: u32,
    timing: &mut Timing<'_>,
) -> Result<(Vec<f32>, Vec<Value>)> {
    let job = timing.job;
    let cancelled = || job.cancelled();
    let scratch = tempfile::tempdir_in(folder)?;
    let stride = width.max(height).div_ceil(768);
    let retained = count(width.div_ceil(stride), height.div_ceil(stride))?
        .checked_mul(sources.len())
        .context("Mosaic calibration dimensions overflow")?;
    ensure!(
        retained <= MAX_LAYER_PIXELS,
        "Mosaic calibration exceeds the bounded retained-plane budget."
    );
    let mut coarse = Vec::new();
    let mut coverage = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let image = aligned(
            &source.path,
            target,
            width,
            height,
            &cancelled,
            Some(&mut *timing),
        )?;
        timing.start(&format!(
            "Preparing mosaic tile {}/{}",
            index + 1,
            sources.len()
        ))?;
        coverage.push(image.iter().filter(|value| value.is_finite()).count());
        coarse.push(samples(&image, width, height, &cancelled)?);
        let mut file = BufWriter::new(File::create(scratch.path().join(format!("{index}.f32")))?);
        for value in image {
            file.write_all(&value.to_le_bytes())?;
        }
        file.flush()?;
        timing.finish()?;
    }
    timing.start("Calibrating mosaic overlaps")?;
    let calibration = calibrate(&coarse, &coverage, &cancelled)?;
    drop(coarse);
    timing.finish()?;
    let n = count(width, height)?;
    let (mut total, mut weights) = (vec![0_f32; n], vec![0_f32; n]);
    let mut tiles = Vec::new();
    for (index, (source, fit)) in sources.iter().zip(calibration).enumerate() {
        check(&cancelled)?;
        timing.start(&format!(
            "Blending mosaic tile {}/{}",
            index + 1,
            sources.len()
        ))?;
        let bytes = fs::read(scratch.path().join(format!("{index}.f32")))?;
        let image: Vec<_> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        drop(bytes);
        let valid: Vec<_> = image.iter().map(|value| value.is_finite()).collect();
        let distance = distances(&valid, width as usize, height as usize, true, &cancelled)?;
        let (gain, offset) = (
            fit["gain"].as_f64().unwrap() as f32,
            fit["offset"].as_f64().unwrap() as f32,
        );
        for i in 0..n {
            if valid[i] {
                let weight = (distance[i] / 64.).min(1.);
                total[i] += (image[i] * gain + offset) * weight;
                weights[i] += weight;
            }
        }
        let mut tile = source.observation.clone();
        tile.as_object_mut()
            .context("Invalid source observation")?
            .extend(fit.as_object().unwrap().clone());
        tile["source"] = json!(reference(session, &source.path)?);
        tile["key"] = json!(layer_key(std::slice::from_ref(&source.observation))?);
        tiles.push(tile);
        timing.finish()?;
    }
    timing.start("Combining mosaic pixels")?;
    for (value, weight) in total.iter_mut().zip(weights) {
        *value = if weight > 0. {
            *value / weight
        } else {
            f32::NAN
        };
    }
    timing.finish()?;
    Ok((total, tiles))
}

pub fn prepare_build(
    session: &Path,
    destination: &Path,
    grid: &Value,
    groups: &[Vec<BuildSource>],
    merge: bool,
    rotation: f64,
    job: &JobHandle,
) -> Result<Value> {
    ensure!(!groups.is_empty(), "Select at least one observation.");
    let (width, height) = (dimension(grid, "width")?, dimension(grid, "height")?);
    let n = count(width, height)?;
    ensure!(
        width.max(height) <= 3072,
        "Build canvas exceeds 3072 pixels."
    );
    let target = Celestial::new(&grid["wcs"], width, height)?;
    job.stage("align", 0., "Planning alignment")?;
    let tasks = alignment_tasks(groups, &target, width, height)?;
    let mut labels: Vec<_> = groups
        .iter()
        .map(|group| {
            let filter = group[0].observation["filter"].as_str().unwrap_or("image");
            if group.len() > 1 {
                format!("Align {filter} ({} tiles)", group.len())
            } else {
                format!("Align {filter}")
            }
        })
        .collect();
    labels.push("Write layers".into());
    let cache = session
        .parent()
        .and_then(Path::parent)
        .context("Project is not in a library")?
        .join("cache/alignment-timings.json");
    let mut timing = Timing::new(job, tasks, labels, cache)?;
    let cancelled = || job.cancelled();
    let (mut luminance, mut counts) = (vec![0_f32; n], vec![0_u32; n]);
    let mut layers = Vec::new();
    let mut keys = HashSet::new();
    for group in groups {
        let observations: Vec<_> = group
            .iter()
            .map(|source| source.observation.clone())
            .collect();
        let key = layer_key(&observations)?;
        ensure!(
            keys.insert(key.clone()),
            "Selected observation IDs collide on disk."
        );
        let first = group.first().context("Empty build group")?;
        let (mut image, tiles) = if group.len() > 1 {
            let (image, tiles) = mosaic(
                group,
                session,
                destination,
                &target,
                width,
                height,
                &mut timing,
            )?;
            (image, Some(tiles))
        } else {
            (
                aligned(
                    &first.path,
                    &target,
                    width,
                    height,
                    &cancelled,
                    Some(&mut timing),
                )?,
                None,
            )
        };
        let valid: Vec<_> = image.iter().map(|value| value.is_finite()).collect();
        let finite = valid.iter().filter(|value| **value).count();
        timing.revise(
            if finite == 0 {
                "normalize-empty"
            } else {
                "normalize"
            },
            if finite == 0 {
                n as f64
            } else {
                sort_work(finite)
            },
        )?;
        timing.start("Normalizing aligned pixels")?;
        let (stats, coverage) = normalized(&mut image, &cancelled)?;
        timing.finish()?;
        timing.start("Writing layer and edge map")?;
        let file = format!("{key}.f16");
        let image = write_half(&destination.join(&file), &image, &cancelled)?;
        let coverage_file = format!("{key}.coverage.u8");
        projects::write_bytes(
            &destination.join(&coverage_file),
            &valid
                .iter()
                .map(|value| u8::from(*value))
                .collect::<Vec<_>>(),
        )?;
        let edge_file = format!("{key}.edge.f16");
        write_half(
            &destination.join(&edge_file),
            &crate::planes::edge_distance(&valid, width, height, &cancelled)?,
            &cancelled,
        )?;
        let mut layer = first.observation.clone();
        layer["key"] = json!(key);
        layer["file"] = json!(file);
        layer["coverage_file"] = json!(coverage_file);
        layer["edge_file"] = json!(edge_file);
        layer["source"] = json!(reference(session, &first.path)?);
        layer["stats"] = stats;
        layer["coverage"] = json!(coverage);
        if let Some(tiles) = tiles {
            layer["tiles"] = json!(tiles);
            layer["feather_pixels"] = json!(64);
        }
        for i in 0..n {
            if valid[i] {
                luminance[i] += image[i];
                counts[i] += 1;
            }
        }
        layers.push(layer);
        timing.finish()?;
    }
    check(&cancelled)?;
    timing.start("Writing luminance and project metadata")?;
    for (value, count) in luminance.iter_mut().zip(&counts) {
        *value = if *count > 0 {
            *value / *count as f32
        } else {
            0.
        };
    }
    write_half(&destination.join("luminance.f16"), &luminance, &cancelled)?;
    let valid: Vec<_> = counts.iter().map(|count| *count > 0).collect();
    write_half(
        &destination.join("luminance.edge.f16"),
        &crate::planes::edge_distance(&valid, width, height, &cancelled)?,
        &cancelled,
    )?;
    let mut meta = json!({
        "version":1,"build_version":BUILD_VERSION,"width":width,"height":height,
        "wcs":target.header,"pixel_scale_arcsec":target.scale*3600.,
        "grid_hash":format!("{:x}",Sha256::digest(serde_json::to_vec(grid)?)),
        "view_rotation_deg":rotation,"layers":layers,"mosaic":merge,
        "luminance":{"file":"luminance.f16","edge_file":"luminance.edge.f16"},
        "built_at":chrono::Utc::now().to_rfc3339()
    });
    if let Some(region) = grid.get("region_sky") {
        meta["region_sky"] = region.clone();
    }
    projects::write_json(&destination.join("meta.json"), &meta)?;
    timing.finish()?;
    timing.save()?;
    Ok(meta)
}

pub fn preview_source(path: &Path, grid: &Value) -> Result<Vec<u8>> {
    let (width, height) = (dimension(grid, "width")?, dimension(grid, "height")?);
    ensure!(width.max(height) <= 1400, "Preview exceeds 1400 pixels.");
    let target = Celestial::new(&grid["wcs"], width, height)?;
    let mut values = aligned(path, &target, width, height, &|| false, None)?;
    let (stats, _) = normalized(&mut values, &|| false)?;
    let white = stats["p999"].as_f64().unwrap_or(1.).max(1e-8) as f32;
    let mut image = image::GrayImage::new(width, height);
    for (pixel, value) in image.pixels_mut().zip(values) {
        *pixel = image::Luma([(255.
            * ((10. * value.max(0.)).asinh() / (10. * white).asinh()).clamp(0., 1.))
        .round() as u8]);
    }
    let mut output = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut output, 85).encode_image(&image)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "small synthetic alignment timing diagnostic; run explicitly in release mode"]
    async fn alignment_timing_warm_forecast_on_synthetic_fits() {
        use crate::native::tests::{header, write_fits};
        for (width, height, tiles) in [(512, 384, 1), (1024, 768, 1), (1024, 768, 3)] {
            let root = tempfile::tempdir().unwrap();
            let library = root.path().canonicalize().unwrap();
            let session = library.join("sessions/science");
            let downloads = library.join("downloads");
            fs::create_dir_all(&session).unwrap();
            fs::create_dir_all(&downloads).unwrap();
            let h = header(width, height, 0.001);
            let values: Vec<_> = (0..height)
                .flat_map(|y| {
                    (0..width).map(move |x| {
                        (x as f32 * 0.03).sin()
                            + (y as f32 * 0.02).cos()
                            + (x + y) as f32 / width as f32
                    })
                })
                .collect();
            let mut sources = Vec::new();
            for tile in 0..tiles {
                let path = downloads.join(format!("source{tile}.fits"));
                write_fits(&path, &values, &h, true);
                sources.push(BuildSource { path, observation:json!({"obs_id":format!("source{tile}"),
                    "collection":"HST","instrument":"ACS","filter":"F606W","proposal":"timing","wavelength_nm":606.}) });
            }
            let grid = source_metadata(&sources[0].path).unwrap();
            let jobs = crate::jobs::Jobs::default();
            let worker = jobs.clone();
            let (send, receive) = std::sync::mpsc::channel();
            jobs.start_kind("build", true, "timing", "Timing diagnostic", move |job| {
                let groups = vec![sources];
                for pass in 0..2 {
                    let destination = session.join(format!("pass{pass}"));
                    fs::create_dir(&destination)?;
                    if pass == 1 {
                        let target = Celestial::new(&grid["wcs"], width, height)?;
                        let tasks = alignment_tasks(&groups, &target, width, height)?;
                        let mut forecast = Timing::new(
                            &job,
                            tasks,
                            vec!["Align F606W".into(), "Write layers".into()],
                            library.join("cache/alignment-timings.json"),
                        )?;
                        forecast.start("Forecast warm alignment")?;
                        let estimate = worker
                            .latest(Some("timing"))
                            .unwrap()
                            .eta_s
                            .context("Warm alignment should have a forecast")?;
                        ensure!(
                            estimate > 0. && estimate.is_finite(),
                            "Invalid warm alignment forecast"
                        );
                        send.send(estimate)?;
                    }
                    let mut pass_grid = grid.clone();
                    if pass == 0 {
                        pass_grid["width"] = json!(width / 2);
                        pass_grid["height"] = json!(height / 2);
                        pass_grid["wcs"]["NAXIS1"] = json!(width / 2);
                        pass_grid["wcs"]["NAXIS2"] = json!(height / 2);
                    }
                    prepare_build(&session, &destination, &pass_grid, &groups, true, 0., &job)?;
                }
                let profile = projects::read_json(
                    &library.join("cache/alignment-timings.json"),
                    Value::Null,
                )?;
                let actual = profile["rates"]
                    .as_object()
                    .context("Missing recorded alignment timings")?
                    .iter()
                    .filter(|(class, _)| class.as_str() != "write-luminance")
                    .map(|(_, rate)| rate["seconds"].as_f64().unwrap())
                    .sum::<f64>();
                send.send(actual)?;
                job.publish(|| Ok(json!({})))
            })
            .unwrap();
            let estimate = receive
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
            let actual = receive
                .recv_timeout(std::time::Duration::from_secs(60))
                .unwrap();
            jobs.cancel_and_wait();
            assert!(actual > 0.);
            println!(
                "ALIGNMENT_BENCH {width}x{height} tiles={tiles} estimate_s={estimate:.4} actual_s={actual:.4} error_pct={:.1}",
                (estimate / actual - 1.) * 100.
            );
        }
    }

    #[test]
    fn normalization_and_connected_overlap_calibration_match_python() {
        let fixtures: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/build-science.json")).unwrap();
        for case in fixtures["normalization"].as_array().unwrap() {
            let mut input: Vec<_> = case["input"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_f64().map(|value| value as f32).unwrap_or(f32::NAN))
                .collect();
            let (stats, coverage) = normalized(&mut input, &|| false).unwrap();
            assert!((coverage - case["coverage"].as_f64().unwrap()).abs() < 1e-12);
            for (key, expected) in case["stats"].as_object().unwrap() {
                let expected = expected.as_f64().unwrap();
                let actual = stats[key].as_f64().unwrap();
                assert!(
                    (actual - expected).abs() < 2e-6 * expected.abs().max(1.),
                    "{key}: {actual} != {expected}"
                );
            }
            for (actual, expected) in input.iter().zip(case["output"].as_array().unwrap()) {
                let expected = expected.as_f64().unwrap() as f32;
                assert!((actual - expected).abs() < 2e-6 * expected.abs().max(1.));
            }
        }
        let mut coarse = Vec::new();
        let mut coverage = Vec::new();
        for (index, (lo, hi)) in [(0, 56), (40, 88), (72, 96), (90, 96)]
            .into_iter()
            .enumerate()
        {
            let image: Vec<_> = (0..64)
                .flat_map(|y| {
                    (0..96).map(move |x| {
                        let base = (x * 13 + y * 7 + (x * y) % 17) as f32;
                        if x < lo || x >= hi || (index == 3 && y >= 8) {
                            return f32::NAN;
                        }
                        match index {
                            0 => base,
                            1 => (base - 7.) / 2.,
                            2 => base * 1.3 + 2.,
                            _ => 100.,
                        }
                    })
                })
                .collect();
            coverage.push(image.iter().filter(|value| value.is_finite()).count());
            coarse.push(samples(&image, 96, 64, &|| false).unwrap());
        }
        let fits = calibrate(&coarse, &coverage, &|| false).unwrap();
        for (actual, expected) in fits.iter().zip(fixtures["calibration"].as_array().unwrap()) {
            assert_eq!(actual["reference"], expected["reference"]);
            assert_eq!(actual["no_overlap"], expected["no_overlap"]);
            for key in ["gain", "offset"] {
                let (actual, expected) = (
                    actual[key].as_f64().unwrap(),
                    expected[key].as_f64().unwrap(),
                );
                assert!(
                    (actual - expected).abs() < 2e-4,
                    "{key}: {actual} != {expected}"
                );
            }
        }
    }

    #[tokio::test]
    async fn staged_build_writes_top_down_half_coverage_edges_and_portable_mosaic_sources() {
        use crate::native::tests::{header, write_fits};
        let root = tempfile::tempdir().unwrap();
        let library = root.path().canonicalize().unwrap();
        let session = library.join("sessions").join("science");
        let downloads = library.join("downloads");
        let destination = session.join("staging");
        fs::create_dir_all(&destination).unwrap();
        fs::create_dir_all(&downloads).unwrap();
        let h = header(64, 48, 0.001);
        let mut values: Vec<_> = (0..48)
            .flat_map(|y| (0..64).map(move |x| (x + 3 * y) as f32))
            .collect();
        values[0] = f32::NAN;
        let mut sources = Vec::new();
        for index in 0..2 {
            let path = downloads.join(format!("source{index}.fits"));
            let image: Vec<_> = values
                .iter()
                .map(|value| if index == 0 { *value } else { *value * 2. + 7. })
                .collect();
            write_fits(&path, &image, &h, true);
            sources.push(BuildSource {path,observation:json!({"obs_id":format!("source{index}"),"collection":"HST","instrument":"ACS","filter":"F606W","proposal":"test","wavelength_nm":606.})});
        }
        let grid = source_metadata(&sources[0].path).unwrap();
        let jobs = crate::jobs::Jobs::default();
        let (sent, received) = std::sync::mpsc::channel();
        let session_copy = session.clone();
        let dest_copy = destination.clone();
        jobs.start_kind("build", true, "science", "Science", move |job| {
            let meta = prepare_build(&session_copy, &dest_copy, &grid, &[sources], true, 0., &job)?;
            job.publish(|| Ok(meta.clone()))?;
            sent.send(meta).unwrap();
            Ok(())
        })
        .unwrap();
        let meta = received
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        jobs.cancel_and_wait();
        let completed = jobs.latest(Some("science")).unwrap();
        assert_eq!(completed.steps.len(), 2);
        assert!(completed.steps.iter().all(|step| step["state"] == "done"));
        assert!(library.join("cache/alignment-timings.json").is_file());
        let layer = &meta["layers"][0];
        let pixels = fs::read(destination.join(layer["file"].as_str().unwrap())).unwrap();
        assert_eq!(pixels.len(), 64 * 48 * 2);
        assert_eq!(pixels, fs::read(destination.join("luminance.f16")).unwrap());
        let coverage =
            fs::read(destination.join(layer["coverage_file"].as_str().unwrap())).unwrap();
        assert_eq!(coverage.iter().filter(|value| **value == 0).count(), 1);
        assert_eq!(coverage[47 * 64], 0);
        let values: Vec<_> = pixels
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| f16::from_bits(u16::from_le_bytes(*bytes)).to_f32())
            .collect();
        assert!(values[0] > values[47 * 64 + 1]);
        assert_eq!(layer["tiles"].as_array().unwrap().len(), 2);
        assert!(
            layer["tiles"][0]["source"]
                .as_str()
                .unwrap()
                .starts_with("library:downloads")
        );
        assert_eq!(
            fs::metadata(destination.join(layer["edge_file"].as_str().unwrap()))
                .unwrap()
                .len(),
            64 * 48 * 2
        );
        assert_eq!(meta["build_version"], BUILD_VERSION);
    }
}
