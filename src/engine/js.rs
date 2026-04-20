use boa_engine::object::FunctionObjectBuilder;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsNativeError, JsValue, NativeFunction, Source};
use serde_json::Value as JsonValue;

use crate::model::LoadedOverride;

pub fn apply_js_override(root: JsonValue, override_item: &LoadedOverride) -> Result<JsonValue, String> {
    let profile_json =
        serde_json::to_string(&root).map_err(|err| format!("encode profile payload: {err}"))?;
    let mut context = Context::default();

    register_native_helpers(&mut context)?;
    context
        .register_global_property(
            js_string!("__profileJson"),
            JsValue::String(js_string!(profile_json.as_str())),
            Attribute::all(),
        )
        .map_err(|err| format!("register __profileJson failed: {err}"))?;
    context
        .register_global_property(
            js_string!("__overridePath"),
            JsValue::String(js_string!(override_item.path.as_str())),
            Attribute::all(),
        )
        .map_err(|err| format!("register __overridePath failed: {err}"))?;

    evaluate(&mut context, helper_script(), "override helper")?;
    evaluate(&mut context, &override_item.content, &override_item.path)?;
    let result = evaluate(
        &mut context,
        r#"
(() => {
  if (typeof main !== "function") {
    throw new Error("JS override must define main(profile)");
  }
  const profile = JSON.parse(__profileJson);
  const nextProfile = main(profile);
  if (nextProfile && typeof nextProfile.then === "function") {
    throw new Error("async main(profile) is not supported");
  }
  return JSON.stringify(nextProfile);
})()
"#,
        "invoke main(profile)",
    )?;

    let serialized = result
        .to_string(&mut context)
        .map_err(|err| format!("stringify JS override result failed: {err}"))?
        .to_std_string_escaped();
    serde_json::from_str(&serialized).map_err(|err| format!("decode JS override result: {err}"))
}

fn register_native_helpers(context: &mut Context) -> Result<(), String> {
    let yaml_parse = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(|_, args, context| {
            let content = args
                .get(0)
                .cloned()
                .unwrap_or_default()
                .to_string(context)?
                .to_std_string_escaped();
            let payload =
                crate::override_engine::yaml::parse_yaml_to_json_string(&content).map_err(js_error)?;
            Ok(JsValue::String(js_string!(payload.as_str())))
        }),
    )
    .name("__yamlParseNative")
    .length(1)
    .build();
    context
        .register_global_property(js_string!("__yamlParseNative"), yaml_parse, Attribute::all())
        .map_err(|err| format!("register __yamlParseNative failed: {err}"))?;

    let yaml_stringify = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(|_, args, context| {
            let content = args
                .get(0)
                .cloned()
                .unwrap_or_default()
                .to_string(context)?
                .to_std_string_escaped();
            let payload = crate::override_engine::yaml::stringify_json_to_yaml_string(&content)
                .map_err(js_error)?;
            Ok(JsValue::String(js_string!(payload.as_str())))
        }),
    )
    .name("__yamlStringifyNative")
    .length(1)
    .build();
    context
        .register_global_property(js_string!("__yamlStringifyNative"), yaml_stringify, Attribute::all())
        .map_err(|err| format!("register __yamlStringifyNative failed: {err}"))?;

    let b64d = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(|_, args, context| {
            let content = args
                .get(0)
                .cloned()
                .unwrap_or_default()
                .to_string(context)?
                .to_std_string_escaped();
            let decoded = base64_decode_string(&content).map_err(js_error)?;
            Ok(JsValue::String(js_string!(decoded.as_str())))
        }),
    )
    .name("__b64dNative")
    .length(1)
    .build();
    context
        .register_global_property(js_string!("__b64dNative"), b64d, Attribute::all())
        .map_err(|err| format!("register __b64dNative failed: {err}"))?;

    let b64e = FunctionObjectBuilder::new(
        context.realm(),
        NativeFunction::from_copy_closure(|_, args, context| {
            let content = args
                .get(0)
                .cloned()
                .unwrap_or_default()
                .to_string(context)?
                .to_std_string_escaped();
            let encoded = base64_encode_string(content.as_bytes());
            Ok(JsValue::String(js_string!(encoded.as_str())))
        }),
    )
    .name("__b64eNative")
    .length(1)
    .build();
    context
        .register_global_property(js_string!("__b64eNative"), b64e, Attribute::all())
        .map_err(|err| format!("register __b64eNative failed: {err}"))?;

    Ok(())
}

fn evaluate(context: &mut Context, source: &str, label: &str) -> Result<JsValue, String> {
    context.eval(Source::from_bytes(source)).map_err(|err| {
        let detail = err.to_string();
        format!("{label} failed: {detail}")
    })
}

fn helper_script() -> &'static str {
    r#"
const yaml = Object.freeze({
  parse(value) {
    return JSON.parse(__yamlParseNative(String(value)));
  },
  stringify(value) {
    return __yamlStringifyNative(JSON.stringify(value));
  }
});
const trimWrap = (value) => {
  if (typeof value !== "string") {
    return value;
  }
  if (value.startsWith("<") && value.endsWith(">")) {
    return value.slice(1, -1);
  }
  return value;
};
const isObject = (item) => item && typeof item === "object" && !Array.isArray(item);
const deepMerge = (target, other, isOverride = true) => {
  for (const key in other) {
    if (isObject(other[key])) {
      if (key.endsWith("!")) {
        const nextKey = trimWrap(key.slice(0, -1));
        target[nextKey] = other[key];
      } else {
        const nextKey = trimWrap(key);
        if (!target[nextKey] || !isObject(target[nextKey])) {
          target[nextKey] = {};
        }
        deepMerge(target[nextKey], other[key], isOverride);
      }
    } else if (Array.isArray(other[key])) {
      if (isOverride && key.startsWith("+")) {
        const nextKey = trimWrap(key.slice(1));
        target[nextKey] = [...other[key], ...(target[nextKey] || [])];
      } else if (isOverride && key.endsWith("+")) {
        const nextKey = trimWrap(key.slice(0, -1));
        target[nextKey] = [...(target[nextKey] || []), ...other[key]];
      } else {
        target[trimWrap(key)] = [...other[key]];
      }
    } else {
      target[trimWrap(key)] = other[key];
    }
  }
  return target;
};
const b64d = (value) => __b64dNative(String(value));
const b64e = (value) => __b64eNative(String(value));
globalThis.yaml = yaml;
globalThis.deepMerge = deepMerge;
globalThis.b64d = b64d;
globalThis.b64e = b64e;
"#
}

fn js_error(message: String) -> boa_engine::JsError {
    JsNativeError::typ().with_message(message).into()
}

fn base64_encode_string(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::new();
    let mut index = 0;
    while index < data.len() {
        let first = data[index];
        let second = if index + 1 < data.len() { data[index + 1] } else { 0 };
        let third = if index + 2 < data.len() { data[index + 2] } else { 0 };
        let value = ((first as u32) << 16) | ((second as u32) << 8) | (third as u32);
        encoded.push(TABLE[((value >> 18) & 0x3F) as usize] as char);
        encoded.push(TABLE[((value >> 12) & 0x3F) as usize] as char);
        encoded.push(if index + 1 < data.len() {
            TABLE[((value >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        encoded.push(if index + 2 < data.len() {
            TABLE[(value & 0x3F) as usize] as char
        } else {
            '='
        });
        index += 3;
    }
    encoded
}

fn base64_decode_string(content: &str) -> Result<String, String> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 4];
    let mut chunk_len = 0usize;
    for byte in content.bytes().filter(|value| !value.is_ascii_whitespace()) {
        chunk[chunk_len] = byte;
        chunk_len += 1;
        if chunk_len == 4 {
            decode_base64_chunk(&chunk, &mut bytes)?;
            chunk_len = 0;
        }
    }
    if chunk_len != 0 {
        return Err("invalid base64 padding".to_string());
    }
    String::from_utf8(bytes).map_err(|err| err.to_string())
}

fn decode_base64_chunk(chunk: &[u8; 4], bytes: &mut Vec<u8>) -> Result<(), String> {
    let mut values = [0u8; 4];
    let mut padding = 0usize;
    for (index, item) in chunk.iter().enumerate() {
        values[index] = match item {
            b'A'..=b'Z' => item - b'A',
            b'a'..=b'z' => item - b'a' + 26,
            b'0'..=b'9' => item - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => {
                padding += 1;
                0
            }
            _ => return Err(format!("invalid base64 character: {}", *item as char)),
        };
    }
    let combined = ((values[0] as u32) << 18)
        | ((values[1] as u32) << 12)
        | ((values[2] as u32) << 6)
        | values[3] as u32;
    bytes.push(((combined >> 16) & 0xFF) as u8);
    if padding < 2 {
        bytes.push(((combined >> 8) & 0xFF) as u8);
    }
    if padding < 1 {
        bytes.push((combined & 0xFF) as u8);
    }
    Ok(())
}
