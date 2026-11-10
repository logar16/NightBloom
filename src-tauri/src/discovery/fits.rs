//! Small, bounded FITS image reader/writer for remote survey cutouts.
//! Archive science files are validated structurally without loading their pixels.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const BLOCK: usize = 2880;
const MAX_PIXELS: usize = 100_000_000;

pub(super) struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<f32>,
    pub header: Value,
}

impl Image {
    pub fn new(width: u32, height: u32, pixels: Vec<f32>, header: Value) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && width as u64 * height as u64 <= MAX_PIXELS as u64,
            "The survey image exceeds the supported pixel limit."
        );
        ensure!(
            pixels.len() == width as usize * height as usize,
            "Invalid FITS image shape."
        );
        Ok(Self {
            width,
            height,
            pixels,
            header,
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Self::decode_inner(bytes, false)
    }

    pub fn decode_tile(bytes: &[u8]) -> Result<Self> {
        if bytes.starts_with(&[0x1f, 0x8b]) {
            let mut decoded = Vec::new();
            flate2::read::GzDecoder::new(bytes)
                .take(super::SURVEY_LIMIT as u64 + 1)
                .read_to_end(&mut decoded)
                .context("Invalid gzip-compressed survey science FITS.")?;
            ensure!(
                decoded.len() <= super::SURVEY_LIMIT,
                "The decompressed survey FITS exceeds the download limit."
            );
            return Self::decode_inner(&decoded, true);
        }
        Self::decode_inner(bytes, true)
    }

    fn decode_inner(bytes: &[u8], tile: bool) -> Result<Self> {
        let mut reader = std::io::Cursor::new(bytes);
        for _ in 0..64 {
            let header = read_header(&mut reader)?;
            let (length, dimensions, bitpix) = data_layout(&header)?;
            let position = reader.position();
            // IRSA HiPS tiles omit final block padding, never the actual pixel bytes.
            let required = if tile { length } else { padded(length)? };
            let end = position
                .checked_add(required)
                .ok_or_else(|| anyhow::anyhow!("Oversized FITS image."))?;
            ensure!(
                end <= bytes.len() as u64,
                "The service returned a truncated FITS image."
            );
            if dimensions.len() == 2
                && dimensions[0] > 0
                && dimensions[1] > 0
                && header
                    .get("XTENSION")
                    .is_none_or(|v| v.as_str() == Some("IMAGE"))
            {
                let width = u32::try_from(dimensions[0])?;
                let height = u32::try_from(dimensions[1])?;
                let count = width as u64 * height as u64;
                ensure!(
                    count <= MAX_PIXELS as u64,
                    "The survey image exceeds the supported pixel limit."
                );
                let scale = header["BSCALE"].as_f64().unwrap_or(1.);
                let zero = header["BZERO"].as_f64().unwrap_or(0.);
                let blank = header["BLANK"].as_i64();
                let stride = bitpix.unsigned_abs() as usize / 8;
                let start = position as usize;
                let data = &bytes[start..start + count as usize * stride];
                let mut pixels = Vec::with_capacity(count as usize);
                for sample in data.chunks_exact(stride) {
                    let value = match bitpix {
                        8 => sample[0] as f64,
                        16 => i16::from_be_bytes(sample.try_into()?) as f64,
                        32 => i32::from_be_bytes(sample.try_into()?) as f64,
                        64 => i64::from_be_bytes(sample.try_into()?) as f64,
                        -32 => f32::from_be_bytes(sample.try_into()?) as f64,
                        -64 => f64::from_be_bytes(sample.try_into()?),
                        _ => bail!("Unsupported survey FITS numeric format."),
                    };
                    pixels.push(if bitpix > 0 && blank.is_some_and(|b| value == b as f64) {
                        f32::NAN
                    } else {
                        (value * scale + zero) as f32
                    });
                }
                ensure!(
                    tile || (header["CTYPE1"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("RA"))
                        && header["CTYPE2"]
                            .as_str()
                            .is_some_and(|s| s.starts_with("DEC")))
                        || (header.get("AMDX1").is_some() && header.get("AMDY1").is_some()),
                    "The service returned a survey image without celestial WCS."
                );
                return Self::new(width, height, pixels, header);
            }
            reader.seek(SeekFrom::Start(position + padded(length)?))?;
            if reader.position() >= bytes.len() as u64 {
                break;
            }
        }
        bail!("The service returned FITS without a supported two-dimensional science image.")
    }

    pub fn encode(&self, provenance: &str) -> Result<Vec<u8>> {
        let mut cards = Vec::new();
        card(&mut cards, "SIMPLE", &json!(true))?;
        card(&mut cards, "BITPIX", &json!(-32))?;
        card(&mut cards, "NAXIS", &json!(2))?;
        card(&mut cards, "NAXIS1", &json!(self.width))?;
        card(&mut cards, "NAXIS2", &json!(self.height))?;
        for (key, value) in self
            .header
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Invalid grid WCS."))?
        {
            if [
                "SIMPLE", "BITPIX", "NAXIS", "NAXIS1", "NAXIS2", "EXTEND", "BSCALE", "BZERO",
                "BLANK", "XTENSION", "PCOUNT", "GCOUNT", "CHECKSUM", "DATASUM", "AIPROV", "END",
                "COMMENT", "HISTORY",
            ]
            .contains(&key.as_str())
            {
                continue;
            }
            if key.len() <= 8 && !value.is_null() && !value.is_array() && !value.is_object() {
                card(&mut cards, key, value)?;
            }
        }
        card(&mut cards, "AIPROV", &json!(provenance))?;
        let mut end = [b' '; 80];
        end[..3].copy_from_slice(b"END");
        cards.extend_from_slice(&end);
        cards.resize(cards.len().div_ceil(BLOCK) * BLOCK, b' ');
        for pixel in &self.pixels {
            cards.extend_from_slice(&pixel.to_be_bytes());
        }
        cards.resize(cards.len().div_ceil(BLOCK) * BLOCK, 0);
        Ok(cards)
    }
}

fn card(out: &mut Vec<u8>, key: &str, value: &Value) -> Result<()> {
    ensure!(key.is_ascii() && key.len() <= 8, "Invalid FITS keyword.");
    let value = match value {
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Bool(v) => if *v { "T" } else { "F" }.into(),
        Value::Number(v) => v.to_string(),
        _ => bail!("Invalid FITS header value."),
    };
    ensure!(
        value.is_ascii() && value.len() <= 70,
        "Invalid FITS header value."
    );
    let text = format!("{key:<8}= {value:>20}");
    let mut buffer = [b' '; 80];
    buffer[..text.len()].copy_from_slice(text.as_bytes());
    out.extend_from_slice(&buffer);
    Ok(())
}

fn read_header(reader: &mut impl Read) -> Result<Value> {
    let mut values = Map::new();
    for block_index in 0..364 {
        let mut block = [0; BLOCK];
        reader.read_exact(&mut block).map_err(|_| {
            anyhow::anyhow!("The service returned an invalid or truncated FITS header.")
        })?;
        ensure!(block.is_ascii(), "Invalid FITS header encoding.");
        if block_index == 0 {
            ensure!(
                &block[..8] == b"SIMPLE  " || &block[..8] == b"XTENSION",
                "The service did not return science FITS."
            );
        }
        for raw in block.as_chunks::<80>().0 {
            let raw = std::str::from_utf8(raw)
                .map_err(|_| anyhow::anyhow!("Invalid FITS header encoding."))?;
            let key = raw[..8].trim();
            if key == "END" {
                return Ok(Value::Object(values));
            }
            if &raw[8..10] != "= " {
                continue;
            }
            let raw = raw[10..].trim_start();
            let value = if let Some(quoted) = raw.strip_prefix('\'') {
                let mut result = String::new();
                let mut chars = quoted.chars().peekable();
                while let Some(c) = chars.next() {
                    if c == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    result.push(c);
                }
                json!(result.trim_end())
            } else {
                let token = raw.split('/').next().unwrap_or("").trim();
                match token {
                    "T" => json!(true),
                    "F" => json!(false),
                    _ => token.parse::<i64>().map(|n| json!(n)).unwrap_or_else(|_| {
                        token
                            .replace(['D', 'd'], "E")
                            .parse::<f64>()
                            .ok()
                            .filter(|n| n.is_finite())
                            .map(|n| json!(n))
                            .unwrap_or(Value::Null)
                    }),
                }
            };
            values.insert(key.into(), value);
        }
    }
    bail!("The service returned an oversized FITS header.")
}

fn data_layout(header: &Value) -> Result<(u64, Vec<u64>, i64)> {
    let bitpix = header["BITPIX"].as_i64().unwrap_or(0);
    ensure!(
        [8, 16, 32, 64, -32, -64].contains(&bitpix),
        "Unsupported FITS numeric format."
    );
    let naxis = header["NAXIS"].as_u64().unwrap_or(u64::MAX);
    ensure!(naxis <= 9, "Invalid FITS dimension count.");
    let mut dimensions = Vec::new();
    let mut count = if naxis == 0 { 0u64 } else { 1u64 };
    for axis in 1..=naxis {
        let size = header[format!("NAXIS{axis}")]
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("Invalid FITS image dimensions."))?;
        count = count
            .checked_mul(size)
            .ok_or_else(|| anyhow::anyhow!("Oversized FITS image."))?;
        dimensions.push(size);
    }
    let groups = header["GCOUNT"].as_u64().unwrap_or(1);
    let parameters = header["PCOUNT"].as_u64().unwrap_or(0);
    let length = count
        .checked_add(parameters)
        .and_then(|n| n.checked_mul(groups))
        .and_then(|n| n.checked_mul(bitpix.unsigned_abs() / 8))
        .ok_or_else(|| anyhow::anyhow!("Oversized FITS image."))?;
    Ok((length, dimensions, bitpix))
}

fn padded(length: u64) -> Result<u64> {
    length
        .checked_add(BLOCK as u64 - 1)
        .map(|n| n / BLOCK as u64 * BLOCK as u64)
        .ok_or_else(|| anyhow::anyhow!("Oversized FITS image."))
}

pub(super) fn image_header(bytes: &[u8]) -> Result<(u32, u32, Value)> {
    let header = read_header(&mut std::io::Cursor::new(bytes))?;
    let (_, dimensions, _) = data_layout(&header)?;
    ensure!(
        header["SIMPLE"] == true && dimensions.len() == 2,
        "MDW returned an unsupported image header."
    );
    let (width, height) = (u32::try_from(dimensions[0])?, u32::try_from(dimensions[1])?);
    ensure!(
        width > 0 && height > 0 && width as u64 * height as u64 <= MAX_PIXELS as u64,
        "MDW image exceeds the supported pixel limit."
    );
    Ok((width, height, header))
}

pub(super) fn image_file_size(bytes: &[u8]) -> Result<u64> {
    let mut reader = std::io::Cursor::new(bytes);
    let header = read_header(&mut reader)?;
    let (length, _, _) = data_layout(&header)?;
    Ok(reader.position() + padded(length)?)
}

pub(super) struct Moc {
    pub cells: Vec<u64>,
    pub galactic: bool,
}

/// Read NUNIQ coverage, retaining the coordinate frame of older Galactic HiPS MOCs.
pub(super) fn moc(bytes: &[u8]) -> Result<Moc> {
    let mut reader = std::io::Cursor::new(bytes);
    for _ in 0..8 {
        let header = read_header(&mut reader)?;
        let (length, _, _) = data_layout(&header)?;
        let start = reader.position();
        ensure!(
            start + padded(length)? <= bytes.len() as u64,
            "Truncated survey coverage map."
        );
        if header["XTENSION"] == "BINTABLE" {
            ensure!(
                header["ORDERING"] == "NUNIQ"
                    && matches!(header["COORDSYS"].as_str(), Some("C" | "G")),
                "Unsupported survey coverage coordinates."
            );
            let fields = header["TFIELDS"].as_u64().unwrap_or(0);
            ensure!(
                fields > 0 && fields <= 256,
                "Invalid survey coverage columns."
            );
            let stride = header["NAXIS1"].as_u64().unwrap_or(0) as usize;
            let rows = header["NAXIS2"].as_u64().unwrap_or(0) as usize;
            let mut offset = 0usize;
            let mut selected = None;
            for index in 1..=fields {
                let form = header[format!("TFORM{index}")]
                    .as_str()
                    .unwrap_or("")
                    .trim();
                let split = form
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(form.len());
                let count = if split == 0 {
                    1usize
                } else {
                    form[..split].parse::<usize>()?
                };
                let kind = form[split..].chars().next().unwrap_or(' ');
                let width = match kind {
                    'L' | 'B' | 'A' => 1,
                    'I' => 2,
                    'J' | 'E' => 4,
                    'K' | 'D' | 'C' | 'P' => 8,
                    'M' | 'Q' => 16,
                    'X' => 0,
                    _ => bail!("Unsupported survey coverage column format."),
                };
                let length = if kind == 'X' {
                    count.div_ceil(8)
                } else {
                    count
                        .checked_mul(width)
                        .ok_or_else(|| anyhow::anyhow!("Invalid coverage column."))?
                };
                let name = header[format!("TTYPE{index}")].as_str().unwrap_or("");
                if ["UNIQ", "NPIX"].contains(&name) {
                    ensure!(
                        count == 1 && matches!(kind, 'J' | 'K'),
                        "Invalid NUNIQ coverage pixels."
                    );
                    selected = Some((offset, width));
                }
                offset = offset
                    .checked_add(length)
                    .ok_or_else(|| anyhow::anyhow!("Invalid coverage row."))?;
            }
            ensure!(
                offset <= stride && stride > 0 && rows <= 4_000_000,
                "Invalid coverage row size."
            );
            let (offset, width) =
                selected.ok_or_else(|| anyhow::anyhow!("Survey coverage has no NUNIQ pixels."))?;
            let mut cells = Vec::with_capacity(rows);
            for row in 0..rows {
                let index = start as usize + row * stride + offset;
                let value = if width == 8 {
                    i64::from_be_bytes(bytes[index..index + 8].try_into()?)
                } else {
                    i32::from_be_bytes(bytes[index..index + 4].try_into()?) as i64
                };
                ensure!(
                    value >= 4 && (value as u64) < (4u64 << 58),
                    "Invalid NUNIQ coverage pixels."
                );
                cells.push(value as u64);
            }
            return Ok(Moc {
                cells,
                galactic: header["COORDSYS"] == "G",
            });
        }
        reader.set_position(start + padded(length)?);
    }
    bail!("The service returned an invalid survey coverage map.")
}

pub(super) fn validate(path: &Path) -> Result<()> {
    let mut reader = std::fs::File::open(path)?;
    let size = reader.metadata()?.len();
    ensure!(
        size > 0 && size % BLOCK as u64 == 0,
        "The service returned incomplete science FITS."
    );
    let mut science = false;
    for index in 0..64 {
        let header = read_header(&mut reader)?;
        ensure!(
            index > 0 || header["SIMPLE"] == true,
            "Invalid primary science FITS header."
        );
        let (length, dimensions, _) = data_layout(&header)?;
        science |= dimensions.len() >= 2
            && dimensions.iter().all(|n| *n > 0)
            && (header.get("XTENSION").is_none()
                || header["XTENSION"] == "IMAGE"
                || header["ZIMAGE"] == true);
        let end = reader
            .stream_position()?
            .checked_add(padded(length)?)
            .ok_or_else(|| anyhow::anyhow!("Oversized FITS image."))?;
        ensure!(end <= size, "The service returned truncated science FITS.");
        if end == size {
            ensure!(
                science,
                "The service returned FITS without a science image."
            );
            return Ok(());
        }
        reader.seek(SeekFrom::Start(end))?;
    }
    bail!("The service returned too many FITS extensions.")
}

#[cfg(test)]
pub(super) fn moc_fixture(cells: &[u64]) -> Vec<u8> {
    moc_fixture_frame(cells, false)
}

#[cfg(test)]
pub(super) fn moc_fixture_frame(cells: &[u64], galactic: bool) -> Vec<u8> {
    let mut result = Vec::new();
    for (key, value) in [
        ("SIMPLE", json!(true)),
        ("BITPIX", json!(8)),
        ("NAXIS", json!(0)),
    ] {
        card(&mut result, key, &value).unwrap();
    }
    let finish = |result: &mut Vec<u8>| {
        let mut end = [b' '; 80];
        end[..3].copy_from_slice(b"END");
        result.extend_from_slice(&end);
        result.resize(result.len().div_ceil(BLOCK) * BLOCK, b' ');
    };
    finish(&mut result);
    for (key, value) in [
        ("XTENSION", json!("BINTABLE")),
        ("BITPIX", json!(8)),
        ("NAXIS", json!(2)),
        ("NAXIS1", json!(8)),
        ("NAXIS2", json!(cells.len())),
        ("PCOUNT", json!(0)),
        ("GCOUNT", json!(1)),
        ("TFIELDS", json!(1)),
        ("TTYPE1", json!("UNIQ")),
        ("TFORM1", json!("1K")),
        ("ORDERING", json!("NUNIQ")),
        ("COORDSYS", json!(if galactic { "G" } else { "C" })),
    ] {
        card(&mut result, key, &value).unwrap();
    }
    finish(&mut result);
    for cell in cells {
        result.extend_from_slice(&cell.to_be_bytes());
    }
    result.resize(result.len().div_ceil(BLOCK) * BLOCK, 0);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpadded_hips_tiles_keep_every_pixel_and_still_reject_pixel_truncation() {
        use std::io::Write;

        let compressed = |bytes: &[u8]| {
            let mut gzip =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gzip.write_all(bytes).unwrap();
            gzip.finish().unwrap()
        };
        let mut pixels = vec![1.25; 512 * 512];
        pixels[512 * 512 - 1] = 987.5;
        let tile = Image::new(512, 512, pixels, json!({})).unwrap();
        let mut bytes = tile.encode("irsa-hips-v1").unwrap();
        bytes.truncate(BLOCK + 512 * 512 * 4);
        let decoded = Image::decode_tile(&bytes).unwrap();
        assert_eq!(decoded.pixels, tile.pixels);
        assert!(Image::decode_tile(&bytes[..bytes.len() - 1]).is_err());
        assert!(Image::decode(&bytes).is_err());
        let gzip = compressed(&bytes);
        assert_eq!(Image::decode_tile(&gzip).unwrap().pixels, tile.pixels);
        assert!(Image::decode_tile(&gzip[..gzip.len() - 1]).is_err());
        assert!(Image::decode_tile(&compressed(&bytes[..bytes.len() - 1])).is_err());
    }

    #[test]
    fn science_round_trip_and_truncation() {
        let image = Image::new(
            2,
            1,
            vec![1.25, f32::NAN],
            json!({
                "CTYPE1":"RA---TAN","CTYPE2":"DEC--TAN","CRPIX1":1.0,"CRPIX2":1.0,
                "CRVAL1":0.0,"CRVAL2":0.0,"CDELT1":-0.1,"CDELT2":0.1
            }),
        )
        .unwrap();
        let bytes = image.encode("stsci-dss-v1").unwrap();
        let parsed = Image::decode(&bytes).unwrap();
        assert_eq!(parsed.pixels[0], 1.25);
        assert!(parsed.pixels[1].is_nan());
        assert_eq!(parsed.header["AIPROV"], "stsci-dss-v1");
        assert!(Image::decode(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn nunique_map_is_spatial_evidence_and_rejects_invalid_cells() {
        assert_eq!(
            moc(&moc_fixture(&[4, 5, 16, 63])).unwrap().cells,
            vec![4, 5, 16, 63]
        );
        assert!(moc(&moc_fixture(&[0])).is_err());
    }
}
