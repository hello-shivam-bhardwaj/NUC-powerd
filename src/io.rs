use std::fs;
use std::path::Path;

use anyhow::{Context, Result};

pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed creating parent dir {}", parent.display()))?;
    }

    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let tmp_name = format!(".{file_name}.tmp.{}", std::process::id());
    let tmp_path = match path.parent() {
        Some(parent) => parent.join(tmp_name),
        None => Path::new(".").join(tmp_name),
    };

    fs::write(&tmp_path, content)
        .with_context(|| format!("failed writing temp file {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "failed replacing {} with {}",
            path.display(),
            tmp_path.display()
        )
    })?;
    Ok(())
}
