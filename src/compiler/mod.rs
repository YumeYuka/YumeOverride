pub mod normalize;
pub mod patch;
pub mod schema;

use serde_json::{Map as JsonMap, Value as JsonValue};
use serde_yaml::Value as YamlValue;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

use crate::engine;
use crate::io::{load_overrides, write_atomic};
use crate::model::{CompileRequest, CompileResult, REQUEST_SCHEMA_VERSION};

pub fn compile_request(
    request: CompileRequest,
    write_output: bool,
) -> Result<CompileResult, String> {
    if request.schema_version != REQUEST_SCHEMA_VERSION {
        return Err(format!(
            "unsupported schema version: {}",
            request.schema_version
        ));
    }

    let source_yaml = fs::read_to_string(&request.profile_path)
        .map_err(|err| format!("read profile yaml: {err}"))?;
    let source_value: YamlValue =
        serde_yaml::from_str(&source_yaml).map_err(|err| format!("parse source yaml: {err}"))?;
    let mut root: JsonValue = serde_json::to_value(source_value)
        .map_err(|err| format!("convert source yaml to json: {err}"))?;

    let loaded_overrides = load_overrides(&request.overrides)?;
    let mut warnings = loaded_overrides.warnings;
    let apply_result = engine::apply_overrides(root, &loaded_overrides.items)?;
    root = apply_result.root;
    warnings.extend(apply_result.warnings);

    let profile_dir = Path::new(&request.profile_dir);
    if !root.is_object() {
        return Err("compiled root config must be an object".to_string());
    }
    patch::patch_static_runtime(&mut root, profile_dir);

    let object = root
        .as_object_mut()
        .ok_or_else(|| "compiled root config must be an object".to_string())?;
    validate_root_config(object)?;
    patch::validate_provider_paths(object, profile_dir)?;

    let final_yaml = serde_yaml::to_string(&normalize::normalize_root(&root))
        .map_err(|err| format!("encode final yaml: {err}"))?;
    let fingerprint = {
        let mut hasher = Sha256::new();
        hasher.update(request.profile_uuid.as_bytes());
        hasher.update(final_yaml.as_bytes());
        format!("{:x}", hasher.finalize())
    };

    if write_output {
        let output_path = request.output_path.trim();
        if output_path.is_empty() {
            return Err("compile mode requires outputPath".to_string());
        }
        write_atomic(Path::new(&output_path), final_yaml.as_bytes())
            .map_err(|err| format!("write runtime yaml: {err}"))?;
    }

    Ok(CompileResult {
        success: true,
        fingerprint,
        final_yaml,
        warnings,
        error: None,
    })
}

fn validate_root_config(object: &JsonMap<String, JsonValue>) -> Result<(), String> {
    validate_geosite_matcher(object)?;
    Ok(())
}

fn validate_geosite_matcher(object: &JsonMap<String, JsonValue>) -> Result<(), String> {
    let Some(value) = object.get("geosite-matcher") else {
        return Ok(());
    };
    let Some(value) = value.as_str() else {
        return Err(
            "geosite-matcher must be a string (supported values: mph, succinct)".to_string(),
        );
    };
    if matches!(value, "mph" | "succinct") {
        return Ok(());
    }
    Err(format!(
        "geosite-matcher must be one of: mph, succinct (got {value})"
    ))
}
