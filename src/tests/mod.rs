use serde_json::{json, Value as JsonValue};
use std::fs;

use crate::compiler::compile_request;
use crate::compiler::normalize::normalize_root;
use crate::model::{CompileRequest, LoadedOverride, REQUEST_SCHEMA_VERSION};
use crate::override_engine;
use crate::override_engine::yaml::add_yaml_tags_to_proxies_short_id;

fn compile_root_with_geosite_matcher(
    value: Option<JsonValue>,
) -> Result<crate::model::CompileResult, String> {
    let temp_dir = std::env::temp_dir().join(format!(
        "yumebox-compiler-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp profile dir");

    let mut root = serde_json::Map::new();
    root.insert("mode".to_string(), JsonValue::String("rule".to_string()));
    if let Some(value) = value {
        root.insert("geosite-matcher".to_string(), value);
    }

    let profile_path = temp_dir.join("profile.yaml");
    let yaml = serde_yaml::to_string(&normalize_root(&JsonValue::Object(root)))
        .expect("serialize profile yaml");
    fs::write(&profile_path, yaml).expect("write profile yaml");

    let request = CompileRequest {
        schema_version: REQUEST_SCHEMA_VERSION,
        profile_uuid: "test-profile".to_string(),
        profile_dir: temp_dir.to_string_lossy().into_owned(),
        profile_path: profile_path.to_string_lossy().into_owned(),
        overrides: Vec::new(),
        output_path: None,
    };

    let result = compile_request(request, false);
    let _ = fs::remove_dir_all(&temp_dir);
    result
}

#[test]
fn yaml_short_id_is_tagged_as_string() {
    let source = r#"
proxies:
  - name: example
    reality-opts:
      short-id: abc123
"#;
    let processed = add_yaml_tags_to_proxies_short_id(source, false);
    assert!(processed.contains("short-id: !!str abc123"));
}

#[test]
fn compile_request_accepts_explicit_geosite_matcher() {
    let result =
        compile_root_with_geosite_matcher(Some(json!("mph"))).expect("compile request should succeed");
    assert!(result.success);
    let root: JsonValue = serde_yaml::from_str(&result.final_yaml).expect("parse final yaml");
    assert_eq!(
        root.get("geosite-matcher").and_then(JsonValue::as_str),
        Some("mph")
    );
}

#[test]
fn compile_request_rejects_invalid_geosite_matcher() {
    let err = compile_root_with_geosite_matcher(Some(json!("invalid")))
        .expect_err("compile request should fail for invalid geosite-matcher");
    assert!(
        err.contains("geosite-matcher must be one of"),
        "unexpected error message: {err}"
    );
}

#[test]
fn yaml_override_is_applied_with_merge_order() {
    let root = json!({
        "mode": "rule",
        "proxies": [{ "name": "A", "type": "http", "server": "one", "port": 80 }]
    });
    let overrides = vec![LoadedOverride {
        path: "test.yaml".to_string(),
        ext: "yaml".to_string(),
        content: "proxies-end:\n  - name: B\n    type: http\n    server: two\n    port: 81\n"
            .to_string(),
    }];
    let result = override_engine::apply_overrides(root, &overrides).expect("apply yaml override");
    let proxies = result
        .get("proxies")
        .and_then(JsonValue::as_array)
        .expect("proxies");
    assert_eq!(proxies.len(), 2);
    assert_eq!(proxies[1].get("name").and_then(JsonValue::as_str), Some("B"));
}

#[test]
fn js_override_can_use_yaml_helpers() {
    let root = json!({ "mode": "rule" });
    let overrides = vec![LoadedOverride {
        path: "test.js".to_string(),
        ext: "js".to_string(),
        content: r#"
function main(profile) {
  const tun = yaml.parse("tun:\n  enable: false\n  stack: mixed\n");
  return deepMerge(profile, tun, true);
}
"#
        .to_string(),
    }];
    let result = override_engine::apply_overrides(root, &overrides).expect("apply js override");
    assert_eq!(result["tun"]["enable"], JsonValue::Bool(false));
    assert_eq!(result["tun"]["stack"], JsonValue::String("mixed".to_string()));
}

#[test]
fn compile_request_normalizes_provider_paths_to_relative_scope() {
    let temp_dir = std::env::temp_dir().join(format!(
        "yumebox-provider-path-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp profile dir");

    let profile_path = temp_dir.join("profile.yaml");
    fs::write(
        &profile_path,
        r#"
mode: rule
rules:
  - RULE-SET,geolocation-!cn,PROXY
rule-providers:
  geolocation-!cn:
    type: http
    url: https://example.com/geolocation-!cn.yaml
    path: Y:/RiderProjects/YumeBox-Desktop/override/providers/rules/geolocation-!cn.yaml
    behavior: domain
    interval: 86400
    format: yaml
"#,
    )
    .expect("write profile yaml");

    let request = CompileRequest {
        schema_version: REQUEST_SCHEMA_VERSION,
        profile_uuid: "test-profile".to_string(),
        profile_dir: temp_dir.to_string_lossy().into_owned(),
        profile_path: profile_path.to_string_lossy().into_owned(),
        overrides: Vec::new(),
        output_path: None,
    };

    let result = compile_request(request, false).expect("compile request should succeed");
    let root: JsonValue = serde_yaml::from_str(&result.final_yaml).expect("parse final yaml");
    assert_eq!(
        root["rule-providers"]["geolocation-!cn"]["path"].as_str(),
        Some("providers/rules/geolocation-!cn.yaml")
    );

    let _ = fs::remove_dir_all(&temp_dir);
}
