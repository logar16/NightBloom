use crate::projects;
use anyhow::{Context, Result, ensure};
use image::{ImageReader, codecs::jpeg::JpegEncoder};
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::PathBuf, time::Duration};
use tokio::sync::Mutex;

const MAX_BYTES: usize = 64 * 1024 * 1024;

pub struct Previews {
    root: PathBuf,
    client: reqwest::Client,
    endpoint: String,
    locks: [Mutex<()>; 64],
}

impl Previews {
    pub fn new(data: &std::path::Path) -> Result<Self> {
        Ok(Self {
            root: data.join("cache").join("previews"),
            client: reqwest::Client::builder()
                .user_agent("NightBloom/0.1.0")
                .connect_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(90))
                .build()?,
            endpoint: "https://mast.stsci.edu/api/v0.1/Download/file".into(),
            locks: std::array::from_fn(|_| Mutex::new(())),
        })
    }

    pub async fn archive(&self, uri: &str, large: bool) -> Result<Vec<u8>> {
        ensure!(uri.starts_with("mast:"), "Expected a MAST preview URI.");
        let digest = Sha256::digest(uri.as_bytes());
        let key = format!("{digest:x}");
        let thumb = self.root.join(format!("{key}.jpg"));
        let full = self.root.join(format!("{key}.large.jpg"));
        let requested = if large { full.clone() } else { thumb.clone() };
        let _guard = self.locks[digest[0] as usize % self.locks.len()]
            .lock()
            .await;
        match tokio::fs::read(&requested).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("Reading cached archive preview"),
        }
        let mut response = self
            .client
            .get(&self.endpoint)
            .query(&[("uri", uri)])
            .send()
            .await
            .context("Downloading archive preview")?
            .error_for_status()?;
        ensure!(
            response
                .content_length()
                .is_none_or(|n| n <= MAX_BYTES as u64),
            "Preview exceeds 64 MiB."
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= MAX_BYTES.saturating_sub(bytes.len()),
                "Preview exceeds 64 MiB."
            );
            bytes.extend_from_slice(&chunk);
        }
        tokio::task::spawn_blocking(move || -> Result<()> {
            let image = ImageReader::new(Cursor::new(bytes))
                .with_guessed_format()?
                .decode()
                .context("Decoding archive preview")?;
            let image = image
                .thumbnail(image.width().min(1400), image.height().min(1400))
                .to_rgb8();
            let mut large = Vec::new();
            JpegEncoder::new_with_quality(&mut large, 88).encode_image(&image)?;
            let image = image::DynamicImage::ImageRgb8(image);
            let image = image.thumbnail(image.width().min(512), image.height().min(512));
            let mut small = Vec::new();
            JpegEncoder::new_with_quality(&mut small, 88).encode_image(&image)?;
            projects::write_bytes(&full, &large)?;
            projects::write_bytes(&thumb, &small)?;
            Ok(())
        })
        .await??;
        tokio::fs::read(&requested)
            .await
            .context("Reading saved archive preview")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn archive_preview_survives_source_and_network_loss() {
        let root = tempfile::tempdir().unwrap();
        let image = image::RgbImage::from_pixel(1200, 800, image::Rgb([240, 120, 40]));
        let mut jpeg = Vec::new();
        JpegEncoder::new_with_quality(&mut jpeg, 88)
            .encode_image(&image)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let count = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.contains("uri=mast%3AHST%2Fproduct%2Fpreview.jpg"));
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        jpeg.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(&jpeg).await.unwrap();
        });
        let mut cache = Previews::new(root.path()).unwrap();
        cache.client = reqwest::Client::builder().no_proxy().build().unwrap();
        cache.endpoint = format!("http://{address}");
        let uri = "mast:HST/product/preview.jpg";
        let first = cache.archive(uri, false).await.unwrap();
        server.await.unwrap();
        assert_eq!(image::load_from_memory(&first).unwrap().width(), 512);
        assert_eq!(cache.archive(uri, false).await.unwrap(), first);
        let large = cache.archive(uri, true).await.unwrap();
        assert_eq!(image::load_from_memory(&large).unwrap().width(), 1200);
        assert!(
            cache
                .archive("https://example.invalid", false)
                .await
                .is_err()
        );
    }
}
