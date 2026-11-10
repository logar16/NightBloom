//! Read-only top-down float16 caches, temporary native float32 planes, and edges.
use anyhow::{Context, Result, bail};
use half::f16;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

pub(crate) fn check_cancelled(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        bail!("render cancelled");
    }
    Ok(())
}

pub(crate) fn pixel_count(width: u32, height: u32) -> Result<usize> {
    if width == 0 || height == 0 {
        bail!("image dimensions must be positive");
    }
    (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| anyhow::anyhow!("image dimensions overflow addressable memory"))
}

pub(crate) fn filled<T: Clone>(len: usize, value: T) -> Result<Vec<T>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(len)
        .context("not enough memory for image buffer")?;
    result.resize(len, value);
    Ok(result)
}

/// Resolve one project asset without accepting absolute paths or traversal.
pub(crate) fn project_file(folder: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if name.is_empty()
        || name.contains(['/', '\\', ':'])
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        bail!("unsafe project plane filename: {name}");
    }
    let candidate = folder.join(path);
    // Followed symlinks must still be confined to this project.
    if candidate.exists()
        && !candidate
            .canonicalize()?
            .starts_with(folder.canonicalize()?)
    {
        bail!("project plane points outside the project: {name}");
    }
    Ok(candidate)
}

enum Samples {
    Half(Vec<u16>),
    Float(Vec<f32>),
}

/// Cached half-float storage is retained until sampling; native float32 stays exact.
pub(crate) struct Plane {
    pub width: u32,
    pub height: u32,
    samples: Samples,
}

impl Plane {
    /// Read only a rectangular range of an existing plane. Native tiled exports
    /// retain exact float32 samples without loading all prepared sources at once.
    pub(crate) fn load_region(
        path: &Path,
        full: (u32, u32),
        region: crate::native::Region,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self> {
        check_cancelled(cancelled)?;
        if region
            .x
            .checked_add(region.width)
            .is_none_or(|r| r > full.0)
            || region
                .y
                .checked_add(region.height)
                .is_none_or(|b| b > full.1)
        {
            bail!("plane window exceeds stored dimensions");
        }
        let n = pixel_count(region.width, region.height)?;
        let float32 = path.extension().is_some_and(|ext| ext == "f32");
        let element = if float32 { 4 } else { 2 };
        let expected = pixel_count(full.0, full.1)?
            .checked_mul(element)
            .context("plane byte count overflow")?;
        let mut file = File::open(path)?;
        if file.metadata()?.len() != expected as u64 {
            bail!("invalid windowed plane dimensions");
        }
        let mut samples = if float32 {
            Samples::Float(Vec::new())
        } else {
            Samples::Half(Vec::new())
        };
        match &mut samples {
            Samples::Float(values) => values.try_reserve_exact(n)?,
            Samples::Half(values) => values.try_reserve_exact(n)?,
        }
        let mut row = vec![0_u8; region.width as usize * element];
        for y in region.y..region.y + region.height {
            check_cancelled(cancelled)?;
            file.seek(SeekFrom::Start(
                (y as u64 * full.0 as u64 + region.x as u64) * element as u64,
            ))?;
            file.read_exact(&mut row)?;
            match &mut samples {
                Samples::Float(values) => values.extend(
                    row.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b)),
                ),
                Samples::Half(values) => values.extend(
                    row.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes(*b)),
                ),
            }
        }
        Ok(Self {
            width: region.width,
            height: region.height,
            samples,
        })
    }

    /// Read little-endian pixels without changing their storage precision.
    /// Only temporary native `.f32` assets select float32; existing cache assets
    /// retain the original half-float loading boundary.
    pub fn load(
        path: &Path,
        width: u32,
        height: u32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self> {
        check_cancelled(cancelled)?;
        let n = pixel_count(width, height)?;
        let float32 = path.extension().is_some_and(|extension| extension == "f32");
        let element_bytes = if float32 { 4 } else { 2 };
        let bytes = n
            .checked_mul(element_bytes)
            .context("plane byte count overflow")?;
        let file =
            File::open(path).with_context(|| format!("cannot read plane {}", path.display()))?;
        if file.metadata()?.len() != bytes as u64 {
            bail!(
                "invalid float{} plane dimensions: {} (expected {bytes} bytes)",
                if float32 { 32 } else { 16 },
                path.display()
            );
        }
        let mut samples = if float32 {
            let mut values = Vec::new();
            values
                .try_reserve_exact(n)
                .context("not enough memory for native science plane")?;
            Samples::Float(values)
        } else {
            let mut bits = Vec::new();
            bits.try_reserve_exact(n)
                .context("not enough memory for science plane")?;
            Samples::Half(bits)
        };
        let mut reader = BufReader::new(file);
        let mut strip = vec![0u8; bytes.min(256 * 1024)];
        let mut offset = 0;
        while offset < n {
            check_cancelled(cancelled)?;
            let count = ((n - offset) * element_bytes).min(strip.len());
            reader.read_exact(&mut strip[..count])?;
            match &mut samples {
                Samples::Half(bits) => bits.extend(
                    strip[..count]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes(*b)),
                ),
                Samples::Float(values) => values.extend(
                    strip[..count]
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|b| f32::from_le_bytes(*b)),
                ),
            }
            offset += count / element_bytes;
        }
        Ok(Self {
            width,
            height,
            samples,
        })
    }

    pub fn from_f32(width: u32, height: u32, samples: Vec<f32>) -> Result<Self> {
        if samples.len() != pixel_count(width, height)? {
            bail!("invalid float32 plane dimensions");
        }
        Ok(Self {
            width,
            height,
            samples: Samples::Float(samples),
        })
    }

    pub fn from_half_values(width: u32, height: u32, samples: Vec<f32>) -> Result<Self> {
        if samples.len() != pixel_count(width, height)? {
            bail!("invalid generated half-float plane dimensions");
        }
        let bits = samples
            .into_iter()
            .map(|v| f16::from_f32(v).to_bits())
            .collect();
        Ok(Self {
            width,
            height,
            samples: Samples::Half(bits),
        })
    }

    fn raw(&self, index: usize) -> f32 {
        match &self.samples {
            Samples::Half(v) => f16::from_bits(v[index]).to_f32(),
            Samples::Float(v) => v[index],
        }
    }

    pub fn value(&self, index: usize) -> f32 {
        let v = self.raw(index);
        if v.is_finite() { v } else { 0. }
    }

    pub fn finite_coverage(&self) -> Vec<bool> {
        (0..self.width as usize * self.height as usize)
            .map(|i| self.raw(i).is_finite())
            .collect()
    }

    /// Float32 bilinear filtering with nearest boundaries, at normalized centres.
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        self.sample_pixels(x * self.width as f32, y * self.height as f32)
    }

    /// Sample physical top-down pixel coordinates, whose centres are i + .5.
    /// Native canvas geometry may be fractional; storage remains an integer grid.
    pub fn sample_pixels(&self, x: f32, y: f32) -> f32 {
        let px = (x - 0.5).clamp(0., self.width as f32 - 1.);
        let py = (y - 0.5).clamp(0., self.height as f32 - 1.);
        let ix = px.floor() as u32;
        let iy = py.floor() as u32;
        let hx = (ix + 1).min(self.width - 1);
        let hy = (iy + 1).min(self.height - 1);
        let (fx, fy) = (px - ix as f32, py - iy as f32);
        let get = |x, y| self.value((y * self.width + x) as usize);
        let a = get(ix, iy);
        let b = get(hx, iy);
        let c = get(ix, hy);
        let d = get(hx, hy);
        (a + (b - a) * fx) * (1. - fy) + (c + (d - c) * fx) * fy
    }
}

pub(crate) fn load_coverage(
    path: &Path,
    width: u32,
    height: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<bool>> {
    let n = pixel_count(width, height)?;
    check_cancelled(cancelled)?;
    let file = File::open(path)?;
    if file.metadata()?.len() != n as u64 {
        bail!("invalid coverage plane dimensions: {}", path.display());
    }
    let mut reader = BufReader::new(file);
    let mut out = filled(n, false)?;
    let mut strip = vec![0u8; n.min(256 * 1024)];
    let mut offset = 0;
    while offset < n {
        check_cancelled(cancelled)?;
        let count = (n - offset).min(strip.len());
        reader.read_exact(&mut strip[..count])?;
        for i in 0..count {
            out[offset + i] = strip[i] != 0;
        }
        offset += count;
    }
    Ok(out)
}

pub(crate) fn fill_small_holes(
    coverage: &mut [bool],
    width: usize,
    height: usize,
    max_area: f64,
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let mut visited = filled(coverage.len(), false)?;
    let mut queue = VecDeque::new();
    let mut small_component = Vec::new();
    for start in 0..coverage.len() {
        if start % width == 0 {
            check_cancelled(cancelled)?;
        }
        if coverage[start] || visited[start] {
            continue;
        }
        queue.clear();
        queue.push_back(start);
        visited[start] = true;
        small_component.clear();
        let mut count = 0usize;
        let mut border = false;
        while let Some(i) = queue.pop_front() {
            if count.is_multiple_of(65536) {
                check_cancelled(cancelled)?;
            }
            count += 1;
            let (x, y) = (i % width, i / width);
            border |= x == 0 || y == 0 || x + 1 == width || y + 1 == height;
            if !border && count as f64 <= max_area {
                small_component.push(i);
            } else {
                small_component.clear();
            }
            let neighbors = [
                (x > 0, i.wrapping_sub(1)),
                (x + 1 < width, i + 1),
                (y > 0, i.wrapping_sub(width)),
                (y + 1 < height, i + width),
            ];
            for (exists, next) in neighbors {
                if exists && !coverage[next] && !visited[next] {
                    visited[next] = true;
                    queue.push_back(next);
                }
            }
        }
        if !border && count as f64 <= max_area {
            for i in &small_component {
                coverage[*i] = true;
            }
        }
    }
    Ok(())
}

// Lower envelope of squared-distance parabolas (exact Euclidean EDT, not a chamfer).
fn distance_line(input: &[f32], output: &mut [f32], sites: &mut [usize], bounds: &mut [f64]) {
    let mut k = 0;
    sites[0] = 0;
    bounds[0] = f64::NEG_INFINITY;
    bounds[1] = f64::INFINITY;
    for q in 1..input.len() {
        let intersection = |p: usize| {
            ((input[q] as f64 + (q as f64).powi(2)) - (input[p] as f64 + (p as f64).powi(2)))
                / (2. * (q - p) as f64)
        };
        let mut s = intersection(sites[k]);
        while s <= bounds[k] && k > 0 {
            k -= 1;
            s = intersection(sites[k]);
        }
        k += 1;
        sites[k] = q;
        bounds[k] = s;
        bounds[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, value) in output.iter_mut().enumerate() {
        while bounds[k + 1] < q as f64 {
            k += 1;
        }
        let delta = q as f64 - sites[k] as f64;
        *value = (delta * delta + input[sites[k]] as f64) as f32;
    }
}

/// Distance to missing coverage, excluding canvas borders and <=4096-pixel holes.
/// Computed only in memory: rendering must never create/update project assets.
pub(crate) fn edge_distance(
    covered: &[bool],
    width: u32,
    height: u32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    edge_distance_scaled(covered, width, height, 1., cancelled)
}

pub(crate) fn edge_distance_scaled(
    covered: &[bool],
    width: u32,
    height: u32,
    scale: f32,
    cancelled: &dyn Fn() -> bool,
) -> Result<Vec<f32>> {
    let n = pixel_count(width, height)?;
    if covered.len() != n {
        bail!("invalid coverage dimensions");
    }
    check_cancelled(cancelled)?;
    if !scale.is_finite() || scale <= 0. {
        bail!("invalid coverage edge sampling scale");
    }
    let cap = (1024. * scale).min(65504.);
    if covered.iter().all(|v| *v) {
        return filled(n, cap);
    }
    let mut coverage = covered.to_vec();
    fill_small_holes(
        &mut coverage,
        width as usize,
        height as usize,
        4096. * (scale as f64).powi(2),
        cancelled,
    )?;
    if coverage.iter().all(|v| *v) {
        return filled(n, cap);
    }
    // Distances above 1024 are capped; a finite sentinel avoids inf-inf in EDT.
    let mut distance: Vec<f32> = coverage
        .iter()
        .map(|v| if *v { 1e12 } else { 0. })
        .collect();
    drop(coverage);
    let max = width.max(height) as usize;
    let mut input = filled(max, 0.)?;
    let mut output = filled(max, 0.)?;
    let mut sites = filled(max, 0usize)?;
    let mut bounds = filled(max + 1, 0_f64)?;
    let (w, h) = (width as usize, height as usize);
    for y in 0..h {
        check_cancelled(cancelled)?;
        distance_line(
            &distance[y * w..(y + 1) * w],
            &mut output[..w],
            &mut sites,
            &mut bounds,
        );
        distance[y * w..(y + 1) * w].copy_from_slice(&output[..w]);
    }
    for x in 0..w {
        check_cancelled(cancelled)?;
        for y in 0..h {
            input[y] = distance[y * w + x];
        }
        distance_line(&input[..h], &mut output[..h], &mut sites, &mut bounds);
        for y in 0..h {
            distance[y * w + x] = output[y].sqrt().min(cap);
        }
    }
    Ok(distance)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn little_endian_half_plane_is_top_down_and_interpolated_in_float32() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("science.f16");
        let mut file = File::create(&path).unwrap();
        for v in [0.125, 0.25, 0.5, 1.] {
            file.write_all(&half::f16::from_f32(v).to_bits().to_le_bytes())
                .unwrap();
        }
        drop(file);
        let plane = Plane::load(&path, 2, 2, &|| false).unwrap();
        assert_eq!(plane.sample(0.25, 0.25), 0.125);
        assert_eq!(plane.sample(0.25, 0.75), 0.5);
        assert_eq!(plane.sample(0.5, 0.5), 0.46875);
        assert!(Plane::load(&path, 3, 2, &|| false).is_err());
        assert!(Plane::load(&path, 2, 2, &|| true).is_err());
        assert!(project_file(dir.path(), "../outside.f16").is_err());
    }

    #[test]
    fn temporary_float32_planes_preserve_precision_coverage_and_exact_length() {
        // Design decision: temporary native .f32 planes must not be requantized
        // to half before PNG16 export; cached .f16 planes keep their old storage.
        let dir = tempfile::tempdir().unwrap();
        let float_path = dir.path().join("native.f32");
        let half_path = dir.path().join("cached.f16");
        let values = [
            0.500_123_44_f32,
            -0.000_123_456_7,
            1.,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.,
            0.25,
        ];
        let float_bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let half_bytes: Vec<u8> = values
            .iter()
            .flat_map(|v| f16::from_f32(*v).to_bits().to_le_bytes())
            .collect();
        std::fs::write(&float_path, &float_bytes).unwrap();
        std::fs::write(&half_path, half_bytes).unwrap();
        let float = Plane::load(&float_path, 4, 2, &|| false).unwrap();
        let half = Plane::load(&half_path, 4, 2, &|| false).unwrap();
        assert_eq!(float.value(0).to_bits(), values[0].to_bits());
        assert_eq!(half.value(0), f16::from_f32(values[0]).to_f32());
        assert_ne!(float.value(0), half.value(0));
        assert_eq!(float.value(1), values[1]);
        assert_eq!(float.sample(0.875, 0.75), 0.25);
        assert_eq!(float.sample(0.875, 0.25), 0.);
        assert_eq!(float.value(4), 0.);
        assert_eq!(float.value(5), 0.);
        assert!(float.sample(0.5, 0.5).is_finite());
        assert_eq!(
            float.finite_coverage(),
            vec![true, true, true, false, false, false, true, true]
        );
        assert!(Plane::load(&float_path, 4, 2, &|| true).is_err());
        assert!(Plane::load(&float_path, 2, 2, &|| false).is_err());
        std::fs::write(&float_path, &float_bytes[..float_bytes.len() - 1]).unwrap();
        assert!(Plane::load(&float_path, 4, 2, &|| false).is_err());
        std::fs::write(&float_path, [&float_bytes[..], &[0u8][..]].concat()).unwrap();
        assert!(Plane::load(&float_path, 4, 2, &|| false).is_err());
    }

    #[test]
    fn missing_samples_do_not_poison_valid_neighbors_or_remove_dark_science() {
        // Design decision: non-finite science denotes missing data, not black success
        // for the entire plane. Finite zeros and negative background remain covered.
        let plane = Plane::from_f32(2, 2, vec![f32::NAN, 0., -0.5, 1.]).unwrap();
        assert_eq!(plane.sample(0.75, 0.75), 1.);
        assert_eq!(plane.sample(0.5, 0.5), 0.125);
        assert_eq!(plane.finite_coverage(), vec![false, true, true, true]);
    }

    #[test]
    fn edge_distance_ignores_canvas_boundary_and_fills_only_small_interior_holes() {
        assert_eq!(
            edge_distance(&[true; 6], 3, 2, &|| false).unwrap(),
            vec![1024.; 6]
        );
        let mut covered = vec![true; 9];
        covered[4] = false;
        assert_eq!(
            edge_distance(&covered, 3, 3, &|| false).unwrap(),
            vec![1024.; 9]
        );
        let distance =
            edge_distance(&[false, true, true, true, true, true], 3, 2, &|| false).unwrap();
        assert_eq!(distance[0], 0.);
        assert_eq!(distance[1], 1.);
        assert!((distance[4] - 2_f32.sqrt()).abs() < 1e-6);
    }
}
