use serde_json::{json, Value as JsonValue};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use crate::compiler::compile_request;
use crate::compiler::normalize::normalize_root;
use crate::engine;
use crate::engine::yaml::add_yaml_tags_to_proxies_short_id;
use crate::model::{CompileRequest, LoadedOverride, REQUEST_SCHEMA_VERSION};

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
        output_path: String::new(),
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
    let result = compile_root_with_geosite_matcher(Some(json!("mph")))
        .expect("compile request should succeed");
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
    let result = engine::apply_overrides(root, &overrides).expect("apply yaml override");
    let proxies = result
        .root
        .get("proxies")
        .and_then(JsonValue::as_array)
        .expect("proxies");
    assert_eq!(proxies.len(), 2);
    assert_eq!(
        proxies[1].get("name").and_then(JsonValue::as_str),
        Some("B")
    );
}

#[test]
fn yaml_override_parse_error_includes_override_path() {
    let root = json!({ "mode": "rule" });
    let overrides = vec![LoadedOverride {
        path: "/tmp/custom-routing.yaml".to_string(),
        ext: "yaml".to_string(),
        content: "proxy-groups:\n  -\n  name: Proxy\n".to_string(),
    }];

    let error = engine::apply_overrides(root, &overrides)
        .expect_err("broken yaml override should fail");
    assert!(
        error.contains("/tmp/custom-routing.yaml"),
        "unexpected error message: {error}"
    );
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
    let result = engine::apply_overrides(root, &overrides).expect("apply js override");
    assert_eq!(result.root["tun"]["enable"], JsonValue::Bool(false));
    assert_eq!(
        result.root["tun"]["stack"],
        JsonValue::String("mixed".to_string())
    );
}

#[test]
fn js_override_supports_async_main_and_writes_log_file() {
    let temp_dir = std::env::temp_dir().join(format!(
        "yumebox-js-async-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp override dir");

    let override_path = temp_dir.join("example.js");
    let overrides = vec![LoadedOverride {
        path: override_path.to_string_lossy().into_owned(),
        ext: "js".to_string(),
        content: r#"
async function main(profile) {
  console.info("boot", { mode: profile.mode });
  await Promise.resolve();
  console.debug("after-await");
  profile.extra = "ok";
  return profile;
}
"#
        .to_string(),
    }];

    let result = engine::apply_overrides(json!({ "mode": "rule" }), &overrides)
        .expect("apply async js override");
    assert!(
        result.warnings.is_empty(),
        "unexpected warnings: {:?}",
        result.warnings
    );
    assert_eq!(result.root["extra"], JsonValue::String("ok".to_string()));

    let log_path = override_path.with_extension("log");
    let log_content = fs::read_to_string(&log_path).expect("read js override log");
    assert!(log_content.contains("[info] 开始执行脚本"));
    assert!(log_content.contains("[info] \"boot\" {\"mode\":\"rule\"}"));
    assert!(log_content.contains("[debug] \"after-await\""));
    assert!(log_content.contains("[info] 脚本执行成功"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn js_override_fetch_helper_reads_http_payload() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind test listener");
    let address = listener.local_addr().expect("listener address");
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let mut buffer = [0u8; 1024];
        let _ = stream.read(&mut buffer);
        let body = r#"{"port":7890}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    });

    let temp_dir = std::env::temp_dir().join(format!(
        "yumebox-js-fetch-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp override dir");
    let override_path = temp_dir.join("fetch.js");
    let overrides = vec![LoadedOverride {
        path: override_path.to_string_lossy().into_owned(),
        ext: "js".to_string(),
        content: format!(
            r#"
async function main(profile) {{
  const response = await fetch("http://{address}/override");
  const payload = await response.json();
  console.log("status", response.status);
  profile.port = payload.port;
  return profile;
}}
"#
        ),
    }];

    let result = engine::apply_overrides(json!({ "mode": "rule" }), &overrides)
        .expect("apply js fetch override");
    assert!(
        result.warnings.is_empty(),
        "unexpected warnings: {:?}",
        result.warnings
    );
    assert_eq!(result.root["port"], JsonValue::from(7890));

    server.join().expect("join http server");
    let log_content =
        fs::read_to_string(override_path.with_extension("log")).expect("read fetch log");
    assert!(log_content.contains("[log] \"status\" 200"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn js_override_failure_is_reported_as_warning_and_keeps_original_profile() {
    let temp_dir = std::env::temp_dir().join(format!(
        "yumebox-js-failure-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp override dir");

    let root = json!({ "mode": "rule", "port": 7890 });
    let override_path = temp_dir.join("broken.js");
    let overrides = vec![LoadedOverride {
        path: override_path.to_string_lossy().into_owned(),
        ext: "js".to_string(),
        content: "function main(profile) { throw new Error('boom'); }".to_string(),
    }];

    let result =
        engine::apply_overrides(root.clone(), &overrides).expect("apply broken js override");
    assert_eq!(result.root, root);
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("skip JS override"));
    assert!(result.warnings[0].contains("boom"));

    let log_content =
        fs::read_to_string(override_path.with_extension("log")).expect("read failure log");
    assert!(log_content.contains("[exception] 脚本执行失败"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn compile_request_emits_warning_for_empty_override_file() {
    let temp_dir = std::env::temp_dir().join(format!(
        "yumebox-empty-override-test-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp profile dir");

    let profile_path = temp_dir.join("profile.yaml");
    fs::write(&profile_path, "mode: rule\n").expect("write profile yaml");
    let empty_override_path = temp_dir.join("empty.js");
    fs::write(&empty_override_path, "  \n").expect("write empty override");

    let request = CompileRequest {
        schema_version: REQUEST_SCHEMA_VERSION,
        profile_uuid: "test-profile".to_string(),
        profile_dir: temp_dir.to_string_lossy().into_owned(),
        profile_path: profile_path.to_string_lossy().into_owned(),
        overrides: vec![crate::model::OverrideSpec {
            path: empty_override_path.to_string_lossy().into_owned(),
            ext: "js".to_string(),
        }],
        output_path: String::new(),
    };

    let result = compile_request(request, false).expect("compile request should succeed");
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("skip empty override file"));

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn compile_request_preserves_existing_provider_path() {
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
    path: providers/rules/geolocation-!cn.yaml
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
        output_path: String::new(),
    };

    let result = compile_request(request, false).expect("compile request should succeed");
    let root: JsonValue = serde_yaml::from_str(&result.final_yaml).expect("parse final yaml");
    assert_eq!(
        root["rule-providers"]["geolocation-!cn"]["path"].as_str(),
        Some("providers/rules/geolocation-!cn.yaml")
    );

    let _ = fs::remove_dir_all(&temp_dir);
}

#[test]
fn compile_request_preserves_existing_dot_provider_path() {
    let temp_dir = std::env::temp_dir().join(format!(
        "yumebox-provider-relative-test-{}",
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
  - RULE-SET,ads_domain,REJECT
rule-providers:
  ads_domain:
    type: http
    url: https://example.com/ads_domain.mrs
    path: ./providers/rules/ads_domain.mrs
    behavior: domain
    interval: 86400
    format: mrs
"#,
    )
    .expect("write profile yaml");

    let request = CompileRequest {
        schema_version: REQUEST_SCHEMA_VERSION,
        profile_uuid: "test-profile".to_string(),
        profile_dir: temp_dir.to_string_lossy().into_owned(),
        profile_path: profile_path.to_string_lossy().into_owned(),
        overrides: Vec::new(),
        output_path: String::new(),
    };

    let result = compile_request(request, false).expect("compile request should succeed");
    let root: JsonValue = serde_yaml::from_str(&result.final_yaml).expect("parse final yaml");
    assert_eq!(
        root["rule-providers"]["ads_domain"]["path"].as_str(),
        Some("./providers/rules/ads_domain.mrs")
    );

    let _ = fs::remove_dir_all(&temp_dir);
}
