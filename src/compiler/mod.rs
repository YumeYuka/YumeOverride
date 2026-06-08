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
use crate::model::{CompileRawResult, CompileRequest, CompileResult, REQUEST_SCHEMA_VERSION};

struct CompiledRoot {
    root: JsonValue,
    warnings: Vec<String>,
}

pub fn compile_request(
    request: CompileRequest,
    write_output: bool,
) -> Result<CompileResult, String> {
    let compiled = compile_root(&request)?;

    let final_yaml = serde_yaml::to_string(&normalize::normalize_root(&compiled.root))
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
        warnings: compiled.warnings,
        error: None,
    })
}

pub fn compile_raw_request(request: CompileRequest) -> Result<CompileRawResult, String> {
    let compiled = compile_root(&request)?;
    let config_raw = serde_json::to_string(&compiled.root)
        .map_err(|err| format!("encode raw config json: {err}"))?;
    let fingerprint = fingerprint_for(request.profile_uuid.as_bytes(), config_raw.as_bytes());
    Ok(CompileRawResult {
        success: true,
        fingerprint,
        config_raw,
        warnings: compiled.warnings,
        error: None,
    })
}

fn compile_root(request: &CompileRequest) -> Result<CompiledRoot, String> {
    if request.schema_version != REQUEST_SCHEMA_VERSION {
        return Err(format!(
            "unsupported schema version: {}",
            request.schema_version
        ));
    }

    let source_yaml = load_source_yaml(request)?;
    let encrypted = request.age_secret_key.is_some();
    let source_value: YamlValue =
        serde_yaml::from_str(&source_yaml).map_err(|err| format!("parse source yaml: {err}"))?;
    let mut root: JsonValue = serde_json::to_value(source_value)
        .map_err(|err| format!("convert source yaml to json: {err}"))?;

    let loaded_overrides = load_overrides(&request.overrides)?;
    let mut warnings = loaded_overrides.warnings;
    let apply_result = engine::apply_overrides(root, &loaded_overrides.items, encrypted)?;
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

    Ok(CompiledRoot { root, warnings })
}

fn load_source_yaml(request: &CompileRequest) -> Result<String, String> {
    let source_bytes =
        fs::read(&request.profile_path).map_err(|err| format!("read profile yaml: {err}"))?;
    let plaintext = if is_age_encrypted(&source_bytes) {
        decrypt_age_source(&source_bytes, request.age_secret_key.as_deref())?
    } else {
        source_bytes
    };
    String::from_utf8(plaintext).map_err(|err| format!("source yaml is not utf-8: {err}"))
}

fn is_age_encrypted(bytes: &[u8]) -> bool {
    bytes.starts_with(b"age-encryption.org/v1")
        || bytes.starts_with(b"-----BEGIN AGE ENCRYPTED FILE-----")
}

fn decrypt_age_source(ciphertext: &[u8], secret_key: Option<&str>) -> Result<Vec<u8>, String> {
    let secret_key = secret_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "age encrypted profile requires ageSecretKey".to_string())?;
    let identities = parse_age_identities(secret_key)?;
    let mut last_error = String::new();
    for identity in identities {
        match age::decrypt(&identity, ciphertext) {
            Ok(plaintext) => return Ok(plaintext),
            Err(err) => last_error = err.to_string(),
        }
    }
    if last_error.is_empty() {
        last_error = "no matching age identity".to_string();
    }
    Err(format!("decrypt age profile: {last_error}"))
}

fn parse_age_identities(secret_keys: &str) -> Result<Vec<age::x25519::Identity>, String> {
    let mut identities = Vec::new();
    for (index, raw_line) in secret_keys.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("AGE-SECRET-KEY-PQ-1") {
            return Err(
                "hybrid age secret keys are not supported by the Rust override decryptor yet"
                    .to_string(),
            );
        }
        let identity = line
            .parse::<age::x25519::Identity>()
            .map_err(|err| format!("parse age secret key at line {}: {err}", index + 1))?;
        identities.push(identity);
    }
    if identities.is_empty() {
        return Err("no supported age secret keys found".to_string());
    }
    Ok(identities)
}

fn fingerprint_for(profile_uuid: &[u8], payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(profile_uuid);
    hasher.update(payload);
    format!("{:x}", hasher.finalize())
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

// C ABI exports for cross-library calls from C++ bridge
use std::ffi::{c_char, CStr, CString};

/// # Safety
/// Caller must pass a valid null-terminated UTF-8 JSON string.
/// Returns the compiled RawConfig JSON as a Rust-allocated CString that must be
/// freed with override_free_string.
#[no_mangle]
pub unsafe extern "C" fn override_compile_raw(request_json: *const c_char) -> *mut c_char {
    if request_json.is_null() {
        return std::ptr::null_mut();
    }
    let json_str = match CStr::from_ptr(request_json).to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null_mut(),
    };
    let request: CompileRequest = match serde_json::from_str(json_str) {
        Ok(r) => r,
        Err(_) => return std::ptr::null_mut(),
    };
    let result = match compile_raw_request(request) {
        Ok(result) => result,
        Err(_) => return std::ptr::null_mut(),
    };
    CString::new(result.config_raw)
        .unwrap_or_default()
        .into_raw()
}

/// # Safety
/// Caller must pass a pointer previously returned by override_compile_raw.
/// Passing any other pointer or a null pointer is undefined behavior.
#[no_mangle]
pub unsafe extern "C" fn override_free_string(s: *mut c_char) {
    if !s.is_null() {
        drop(CString::from_raw(s));
    }
}
