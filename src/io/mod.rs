use std::fs;
use std::path::Path;

use crate::model::{LoadedOverride, OverrideSpec};

pub fn load_overrides(overrides: &[OverrideSpec]) -> Result<Vec<LoadedOverride>, String> {
    let mut loaded = Vec::new();
    for override_spec in overrides {
        let content = fs::read_to_string(&override_spec.path)
            .map_err(|err| format!("read override file {}: {err}", override_spec.path))?;
        if content.trim().is_empty() {
            continue;
        }
        loaded.push(LoadedOverride {
            path: override_spec.path.clone(),
            ext: override_spec.ext.trim().to_ascii_lowercase(),
            content,
        });
    }
    Ok(loaded)
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "runtime output path has no parent".to_string())?;
    fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    let tmp = path.with_extension("yaml.tmp");
    fs::write(&tmp, bytes).map_err(|err| err.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|err| err.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|err| err.to_string())?;
    Ok(())
}
