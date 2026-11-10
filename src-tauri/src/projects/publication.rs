use super::*;
use anyhow::ensure;

const JOURNAL: &str = ".native-publication";
const HISTORY: [&str; 4] = ["edits.json", "edits.rev.json", "markers.json", "grid.json"];
const ALLOWED: [&str; 11] = [
    "edits.json",
    "edits.rev.json",
    "markers.json",
    "grid.json",
    "view.json",
    "snapshot-view.json",
    "snapshot-wcs.json",
    "snapshot.jpg",
    "catalog.json",
    "selection.json",
    "session.json",
];

fn journal_path(session: &Path) -> Result<PathBuf> {
    let path = session.join(JOURNAL);
    if path.exists() {
        ensure!(
            path.canonicalize()?.parent() == Some(session),
            "Unsafe publication journal"
        );
    }
    Ok(path)
}

fn manifest(path: &Path) -> Result<Value> {
    let value = read_json(&path.join("manifest.json"), Value::Null)?;
    for file in value["files"]
        .as_array()
        .context("Invalid publication manifest")?
    {
        ensure!(
            ALLOWED.contains(&file["name"].as_str().unwrap_or("")),
            "Unsafe publication filename"
        );
    }
    if let Some(archive) = value["archive"].as_str() {
        ensure!(
            safe_name(archive)
                && archive
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c)),
            "Unsafe build history ID"
        );
    }
    Ok(value)
}

fn rollback(session: &Path, path: &Path, value: &Value) -> Result<()> {
    if value["project"].as_bool() == Some(true) {
        let staged = path.join("after").join("project");
        let original = path.join("before").join("project");
        let current = session.join("project");
        if !staged.exists() && current.exists() {
            ensure!(
                current.canonicalize()?.parent() == Some(session),
                "Unsafe active project"
            );
            fs::rename(&current, &staged).context("Returning interrupted new build to staging")?;
        }
        if original.exists() {
            fs::rename(original, current).context("Restoring interrupted previous build")?;
        }
    }
    for file in value["files"].as_array().unwrap() {
        let name = file["name"].as_str().unwrap();
        if file["existed"].as_bool() == Some(true) {
            write_bytes(
                &session.join(name),
                &fs::read(path.join("before").join(name))?,
            )?;
        } else if session.join(name).exists() {
            fs::remove_file(session.join(name))?;
        }
    }
    Ok(())
}

fn finish(session: &Path, path: &Path, value: &Value) -> Result<()> {
    if let Some(archive) = value["archive"].as_str() {
        let before = path.join("before");
        if before.join("project").exists() {
            let builds = session.join("builds");
            fs::create_dir_all(&builds)?;
            ensure!(
                builds.canonicalize()?.parent() == Some(session),
                "Unsafe build history directory"
            );
            fs::rename(before, builds.join(archive)).context("Archiving previous build")?;
        }
    }
    fs::remove_dir_all(path)?;
    Ok(())
}

/// Recover a process interruption before exposing a project's files.
pub fn recover_publication(session: &Path) -> Result<()> {
    let path = journal_path(session)?;
    if !path.exists() {
        return Ok(());
    }
    if !path.join("manifest.json").exists() {
        // The journal is written last, before any active file is changed.
        fs::remove_dir_all(path)?;
        return Ok(());
    }
    let value = manifest(&path)?;
    if path.join("committed.json").exists() {
        if let Err(error) = finish(session, &path, &value) {
            eprintln!("NightBloom publication is committed; cleanup remains pending: {error:#}");
        }
    } else {
        rollback(session, &path, &value)?;
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

/// Caller holds the project lock and arbitrates cancellation before entering.
pub fn publish_bundle(
    session: &Path,
    updates: &[(&str, Option<Vec<u8>>)],
    new_project: Option<&Path>,
    archive: bool,
) -> Result<()> {
    recover_publication(session)?;
    let path = journal_path(session)?;
    fs::create_dir(&path)?;
    let before = path.join("before");
    let after = path.join("after");
    fs::create_dir(&before)?;
    fs::create_dir(&after)?;
    let prepared = (|| -> Result<Value> {
        let mut files = Vec::new();
        let mut names = std::collections::HashSet::new();
        for (name, bytes) in updates {
            ensure!(
                ALLOWED.contains(name) && names.insert(*name),
                "Invalid publication filename"
            );
            let original = session.join(name);
            let existed = original.try_exists()?;
            if existed {
                fs::copy(&original, before.join(name))?;
            }
            if let Some(bytes) = bytes {
                write_bytes(&after.join(name), bytes)?;
            }
            files.push(json!({"name":name,"existed":existed,"remove":bytes.is_none()}));
        }
        let had_project = session.join("project").try_exists()?;
        if new_project.is_some() && had_project {
            ensure!(
                session.join("project").canonicalize()?.parent() == Some(session),
                "Unsafe active project"
            );
        }
        let archive = if archive && had_project {
            for name in HISTORY {
                if !names.contains(name) && session.join(name).try_exists()? {
                    fs::copy(session.join(name), before.join(name))?;
                }
            }
            Some(format!(
                "{}-{}",
                Utc::now().format("%Y%m%dT%H%M%SZ"),
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ))
        } else {
            None
        };
        if let Some(project) = new_project {
            fs::rename(project, after.join("project"))?;
        }
        let value = json!({"files":files,"project":new_project.is_some(),"archive":archive});
        write_json(&path.join("manifest.json"), &value)?;
        Ok(value)
    })();
    let value = match prepared {
        Ok(value) => value,
        Err(error) => {
            if let Err(cleanup) = fs::remove_dir_all(&path) {
                return Err(error.context(format!("Publication staging cleanup failed: {cleanup}")));
            }
            return Err(error);
        }
    };
    let applied = (|| -> Result<()> {
        if new_project.is_some() {
            let current = session.join("project");
            if current.exists() {
                fs::rename(&current, before.join("project"))?;
            }
            fs::rename(after.join("project"), &current)?;
        }
        for (name, bytes) in updates {
            if let Some(bytes) = bytes {
                write_bytes(&session.join(name), bytes)?;
            } else if session.join(name).exists() {
                fs::remove_file(session.join(name))?;
            }
        }
        write_json(&path.join("committed.json"), &json!({"committed":true}))?;
        Ok(())
    })();
    if let Err(error) = applied {
        if let Err(rollback) = rollback(session, &path, &value) {
            return Err(error.context(format!(
                "Rollback failed: {rollback:#}; recoverable journal retained at {}",
                path.display()
            )));
        }
        fs::remove_dir_all(&path)?;
        return Err(error);
    }
    if let Err(error) = finish(session, &path, &value) {
        eprintln!("NightBloom publication completed; history cleanup will be retried: {error:#}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_publication_restores_previous_files_and_build() {
        let root = tempfile::tempdir().unwrap();
        let session = root.path().canonicalize().unwrap();
        let path = session.join(JOURNAL);
        fs::create_dir_all(path.join("before").join("project")).unwrap();
        fs::create_dir_all(path.join("after")).unwrap();
        fs::write(
            path.join("before").join("project").join("old"),
            b"preserved",
        )
        .unwrap();
        write_json(
            &path.join("before").join("edits.json"),
            &json!({"old":true}),
        )
        .unwrap();
        fs::create_dir(session.join("project")).unwrap();
        fs::write(session.join("project").join("new"), b"new").unwrap();
        write_json(&session.join("edits.json"), &json!({"old":false})).unwrap();
        write_json(
            &path.join("manifest.json"),
            &json!({
                "files":[{"name":"edits.json","existed":true,"remove":false}],
                "project":true,"archive":"saved"
            }),
        )
        .unwrap();
        recover_publication(&session).unwrap();
        assert_eq!(
            fs::read(session.join("project").join("old")).unwrap(),
            b"preserved"
        );
        assert!(!session.join("project").join("new").exists());
        assert_eq!(
            read_json(&session.join("edits.json"), Value::Null).unwrap(),
            json!({"old":true})
        );
        assert!(!path.exists());
    }

    #[test]
    fn committed_publication_keeps_new_build_and_archives_old_look() {
        let root = tempfile::tempdir().unwrap();
        let session = root.path().canonicalize().unwrap();
        fs::create_dir(session.join("project")).unwrap();
        fs::write(session.join("project").join("old"), b"old").unwrap();
        write_json(&session.join("edits.json"), &json!({"look":"old"})).unwrap();
        let stage = tempfile::tempdir_in(&session).unwrap();
        let new_project = stage.path().join("project");
        fs::create_dir(&new_project).unwrap();
        fs::write(new_project.join("new"), b"new").unwrap();
        publish_bundle(
            &session,
            &[("edits.json", Some(br#"{"look":"new"}"#.to_vec()))],
            Some(&new_project),
            true,
        )
        .unwrap();
        assert!(session.join("project").join("new").exists());
        let archive = fs::read_dir(session.join("builds"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(archive.join("project").join("old")).unwrap(),
            b"old"
        );
        assert_eq!(
            read_json(&archive.join("edits.json"), Value::Null).unwrap(),
            json!({"look":"old"})
        );
        assert!(!session.join(JOURNAL).exists());
    }
}
