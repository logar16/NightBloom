use anyhow::{Context, Result};
use astroimp_desktop::{jobs::JobHandle, native};
use serde_json::Value;
use std::{fs, path::PathBuf};

fn main() -> Result<()> {
    let manifest = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .context("Expected input manifest")?,
    );
    let input: Value = serde_json::from_slice(&fs::read(manifest)?)?;
    let sources = input["sources"]
        .as_array()
        .context("Missing sources")?
        .iter()
        .map(|source| {
            Ok(native::BuildSource {
                observation: source["observation"].clone(),
                path: PathBuf::from(source["path"].as_str().context("Missing source path")?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let destination = PathBuf::from(
        input["destination"]
            .as_str()
            .context("Missing destination")?,
    );
    fs::create_dir_all(&destination)?;
    let meta = native::prepare_build(
        &PathBuf::from(
            input["session"]
                .as_str()
                .context("Missing fixture session")?,
        ),
        &destination,
        &input["grid"],
        &[sources],
        input["merge"].as_bool().context("Missing merge choice")?,
        0.,
        &JobHandle::untracked(),
    )?;
    println!("{}", serde_json::to_string_pretty(&meta)?);
    Ok(())
}
