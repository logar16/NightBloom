//! PNG iTXt pointers; encoded image data is copied, never decoded or quantized.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
};
use tempfile::NamedTempFile;

const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const PREFIX: &[u8] = b"nightbloom\0\0\0\0\0";

pub fn link(project: &str, checkpoint: &str) -> Value {
    json!({"version":1,"project":project,"checkpoint":checkpoint})
}

pub fn chunk(value: &Value) -> Result<Vec<u8>> {
    let mut body = PREFIX.to_vec();
    body.extend(serde_json::to_vec(value)?);
    let mut bytes = (body.len() as u32).to_be_bytes().to_vec();
    bytes.extend(b"iTXt");
    bytes.extend(body);
    bytes.extend(crc32fast::hash(&bytes[4..]).to_be_bytes());
    Ok(bytes)
}

pub fn read(path: &Path) -> Result<Option<Value>> {
    let mut input = BufReader::new(File::open(path)?);
    let mut signature = [0; 8];
    if input.get_ref().metadata()?.len() < 8 {
        return Ok(None);
    }
    input.read_exact(&mut signature)?;
    if signature != SIGNATURE {
        return Ok(None);
    }
    loop {
        let mut header = [0; 8];
        input.read_exact(&mut header)?;
        let size = u32::from_be_bytes(header[..4].try_into().unwrap());
        let kind = &header[4..];
        if kind == b"IDAT" || kind == b"IEND" {
            return Ok(None);
        }
        if kind == b"iTXt" && size <= 4096 {
            let mut data = vec![0; size as usize];
            input.read_exact(&mut data)?;
            let mut crc = [0; 4];
            input.read_exact(&mut crc)?;
            if data.starts_with(b"nightbloom\0") {
                ensure!(
                    data.starts_with(PREFIX),
                    "Unsupported PNG saved-look metadata."
                );
                let mut hash = crc32fast::Hasher::new();
                hash.update(kind);
                hash.update(&data);
                ensure!(
                    hash.finalize() == u32::from_be_bytes(crc),
                    "Damaged PNG saved-look metadata."
                );
                return Ok(Some(serde_json::from_slice(&data[PREFIX.len()..])?));
            }
        } else {
            input.seek(SeekFrom::Current(i64::from(size) + 4))?;
        }
    }
}

pub fn attach(source: NamedTempFile, value: &Value) -> Result<NamedTempFile> {
    let mut input = BufReader::new(File::open(source.path())?);
    let mut header = [0; 33];
    if input.get_ref().metadata()?.len() < 8 {
        return Ok(source);
    }
    input.read_exact(&mut header[..8])?;
    if &header[..8] != SIGNATURE {
        return Ok(source);
    }
    input.read_exact(&mut header[8..])?;
    ensure!(&header[8..16] == b"\0\0\0\rIHDR", "Invalid PNG header.");
    if let Some(existing) = read(source.path())? {
        ensure!(
            existing == *value,
            "PNG saved-look link does not match this export."
        );
        return Ok(source);
    }
    let mut output =
        NamedTempFile::new_in(source.path().parent().context("Missing export folder")?)?;
    output.write_all(&header)?;
    output.write_all(&chunk(value)?)?;
    std::io::copy(&mut input, &mut output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageBuffer, ImageFormat, Rgb};

    #[test]
    fn rgb16_link_preserves_encoding_is_idempotent_and_survives_clipboard_conversion() {
        let source = tempfile::Builder::new().suffix(".png").tempfile().unwrap();
        DynamicImage::ImageRgb16(ImageBuffer::from_pixel(4, 3, Rgb([1234, 32768, 65535])))
            .write_to(&mut File::create(source.path()).unwrap(), ImageFormat::Png)
            .unwrap();
        let original = std::fs::read(source.path()).unwrap();
        let pointer = link("alpha", &"a".repeat(64));
        let annotated = attach(source, &pointer).unwrap();
        let encoded = std::fs::read(annotated.path()).unwrap();
        assert_eq!(encoded[24], 16);
        assert_eq!(&encoded[..33], &original[..33]);
        assert_eq!(
            &encoded[33 + chunk(&pointer).unwrap().len()..],
            &original[33..]
        );
        let annotated = attach(annotated, &pointer).unwrap();
        assert_eq!(std::fs::read(annotated.path()).unwrap(), encoded);
        assert_eq!(read(annotated.path()).unwrap(), Some(pointer.clone()));
        assert_eq!(
            image::load_from_memory(&encoded)
                .unwrap()
                .to_rgb16()
                .get_pixel(0, 0)
                .0,
            [1234, 32768, 65535]
        );
        let cache = tempfile::tempdir().unwrap();
        let copied = cache.path().join("copied.png");
        std::fs::copy(annotated.path(), &copied).unwrap();
        let clipboard = crate::gallery::preview(&copied, cache.path(), "clipboard").unwrap();
        let copy = cache.path().join("renamed.png");
        std::fs::write(&copy, clipboard).unwrap();
        assert_eq!(read(&copy).unwrap(), Some(pointer));
        assert!(attach(annotated, &link("beta", &"b".repeat(64))).is_err());
    }
}
