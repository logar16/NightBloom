//! Strong digests reused only while filesystem file identity/change tokens match.
use super::check;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(super) struct Fingerprints {
    entries: HashMap<PathBuf, Value>,
    used: HashSet<PathBuf>,
    #[cfg(test)]
    pub hashed_bytes: u64,
}

impl Fingerprints {
    pub fn from_inputs(inputs: &Value) -> Self {
        let mut memo = Self::default();
        for value in inputs.as_array().into_iter().flatten() {
            if let Some(path) = value["path"].as_str()
                && value["sha256"].as_str().is_some_and(|s| s.len() == 64)
            {
                memo.entries.insert(PathBuf::from(path), value.clone());
            }
        }
        memo
    }

    pub fn from_cache(cache: &Path) -> Result<Self> {
        let saved = crate::projects::read_json(&cache.join("latest.json"), Value::Null)?;
        Ok(Self::from_inputs(&saved["fingerprints"]))
    }

    pub fn snapshot(&self) -> Value {
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .filter(|(path, _)| self.used.contains(*path))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        json!(
            entries
                .into_iter()
                .map(|(_, value)| value)
                .collect::<Vec<_>>()
        )
    }

    pub fn fingerprint(&mut self, path: &Path, cancelled: &dyn Fn() -> bool) -> Result<Value> {
        check(cancelled)?;
        let path = path
            .canonicalize()
            .with_context(|| format!("Missing native input: {}", path.display()))?;
        let mut file = File::open(&path)?;
        self.used.insert(path.clone());
        let before = token(&file)?;
        if let Some(token) = &before
            && let Some(saved) = self.entries.get(&path)
            && saved["change_token"] == *token
        {
            return Ok(saved.clone());
        }
        let metadata = file.metadata()?;
        let mut digest = Sha256::new();
        let mut bytes = vec![0_u8; 256 * 1024];
        loop {
            check(cancelled)?;
            let n = file.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            digest.update(&bytes[..n]);
            #[cfg(test)]
            {
                self.hashed_bytes += n as u64;
            }
        }
        check(cancelled)?;
        ensure!(
            before == token(&file)?
                && metadata.len() == file.metadata()?.len()
                && metadata.modified()? == file.metadata()?.modified()?,
            "Native input changed while validating provenance"
        );
        let sha256 = format!("{:x}", digest.finalize());
        let value = json!({"path":path,"bytes":metadata.len(),
            "change_token":before,"sha256":sha256});
        // Without a reliable change token, retain strong validation by rehashing
        // on every use. Never degrade to a length/mtime-only cache hit.
        if before.is_some() {
            self.entries.insert(path, value.clone());
        } else {
            self.entries.remove(&path);
        }
        Ok(value)
    }
}

#[cfg(windows)]
fn token(file: &File) -> Result<Option<Value>> {
    use std::{ffi::c_void, os::windows::io::AsRawHandle};
    #[repr(C)]
    #[derive(Default)]
    struct Basic {
        creation: i64,
        access: i64,
        write: i64,
        change: i64,
        attributes: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct Id {
        volume: u64,
        id: [u8; 16],
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            handle: *mut c_void,
            class: i32,
            info: *mut c_void,
            size: u32,
        ) -> i32;
    }
    let mut basic = Basic::default();
    let mut id = Id::default();
    // SAFETY: valid open file handle and correctly sized/layouted Windows
    // FILE_BASIC_INFO / FILE_ID_INFO output buffers; neither call writes files.
    let supported = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            0,
            (&mut basic as *mut Basic).cast(),
            std::mem::size_of::<Basic>() as u32,
        ) != 0
            && GetFileInformationByHandleEx(
                file.as_raw_handle(),
                18,
                (&mut id as *mut Id).cast(),
                std::mem::size_of::<Id>() as u32,
            ) != 0
    };
    if !supported {
        return Ok(None);
    }
    Ok(Some(json!({"volume":id.volume.to_string(),"file_id":id.id,
        "creation":basic.creation,"write":basic.write,"change":basic.change,
        "attributes":basic.attributes,"bytes":file.metadata()?.len()})))
}

#[cfg(unix)]
fn token(file: &File) -> Result<Option<Value>> {
    use std::os::unix::fs::MetadataExt;
    let meta = file.metadata()?;
    Ok(Some(
        json!({"device":meta.dev(),"inode":meta.ino(),"bytes":meta.len(),
        "mtime":meta.mtime(),"mtime_ns":meta.mtime_nsec(),
        "ctime":meta.ctime(),"ctime_ns":meta.ctime_nsec()}),
    ))
}

#[cfg(not(any(windows, unix)))]
fn token(_: &File) -> Result<Option<Value>> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::Fingerprints;
    use std::{
        fs::{self, File, FileTimes, OpenOptions},
        io::Write,
    };

    #[test]
    fn digest_reuse_detects_restored_mtime_replacement_and_missing_inputs() {
        // Design decision: length/mtime alone is not an unchanged-file identity.
        // Reuse the strong digest only while file identity AND change time match.
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("science.fits");
        fs::write(&path, b"abc").unwrap();
        let mut memo = Fingerprints::default();
        let first = memo.fingerprint(&path, &|| false).unwrap();
        assert_eq!(
            first["sha256"],
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(memo.hashed_bytes, 3);
        assert_eq!(first, memo.fingerprint(&path, &|| false).unwrap());
        assert_eq!(
            memo.hashed_bytes, 3,
            "unchanged inputs must perform no content reads"
        );
        let saved = fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        fs::write(&path, b"azc").unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(saved))
            .unwrap();
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), saved);
        let changed = memo.fingerprint(&path, &|| false).unwrap();
        assert_ne!(first, changed);
        assert_ne!(first["sha256"], changed["sha256"]);
        assert_eq!(memo.hashed_bytes, 6);
        let mut persisted = Fingerprints::from_inputs(&serde_json::json!([changed.clone()]));
        assert_eq!(changed, persisted.fingerprint(&path, &|| false).unwrap());
        assert_eq!(persisted.hashed_bytes, 0);
        let replacement = root.path().join("replacement");
        File::create(&replacement)
            .unwrap()
            .write_all(b"azc")
            .unwrap();
        fs::remove_file(&path).unwrap();
        fs::rename(replacement, &path).unwrap();
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(saved))
            .unwrap();
        let replaced = persisted.fingerprint(&path, &|| false).unwrap();
        assert_ne!(changed, replaced);
        assert_eq!(changed["sha256"], replaced["sha256"]);
        assert_eq!(persisted.hashed_bytes, 3);
        fs::remove_file(path.clone()).unwrap();
        assert!(persisted.fingerprint(&path, &|| false).is_err());
        assert!(persisted.fingerprint(&path, &|| true).is_err());
    }
}
