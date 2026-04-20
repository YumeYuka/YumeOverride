pub mod js;
pub mod yaml;

use serde_json::Value as JsonValue;

use crate::compiler::patch::apply_override_document;
use crate::model::LoadedOverride;

pub fn apply_overrides(mut root: JsonValue, overrides: &[LoadedOverride]) -> Result<JsonValue, String> {
    for override_item in overrides {
        match override_item.ext.as_str() {
            "yaml" | "yml" => {
                let patch = yaml::parse_yaml_override(&override_item.content)?;
                apply_override_document(&mut root, &patch);
            }
            "js" => {
                root = js::apply_js_override(root, override_item)?;
            }
            other => {
                return Err(format!("unsupported override extension: {other}"));
            }
        }
    }
    Ok(root)
}
