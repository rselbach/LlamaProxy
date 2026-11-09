use super::{BRIDGE_PATH, MODEL_PREFIX};
use serde_json::json;
use serde_norway::Value;
use std::{fs, path::Path};

const SECTION: &str = "claude-api-key";

pub(super) fn write(path: &Path, base_url: &str, key: &str, models: &[&str]) -> Result<(), String> {
    let _guard = crate::lock_core_config_file()?;
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Could not read Claude CLI routing configuration: {error}"
            ))
        }
    };
    #[cfg(unix)]
    if !models.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let directory = path
            .parent()
            .ok_or_else(|| "Claude CLI routing configuration has no directory".to_string())?;
        // The adapter key is in config.yaml, and both the core and the app replace that file.
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!("Could not protect the Claude CLI routing directory: {error}")
        })?;
    }
    if let Some(updated) = crate::patch_core_yaml_document(&content, |document| {
        configure(document, base_url, key, models)
    })? {
        crate::write_yaml_if_changed(path, &updated)?;
    }
    Ok(())
}

// Ownership is scoped to the adapter's loopback path, so native Claude
// credentials and user-created entries survive.
fn managed(entry: &Value) -> bool {
    entry
        .get("base-url")
        .and_then(Value::as_str)
        .and_then(|raw| reqwest::Url::parse(raw).ok())
        .is_some_and(|url| {
            url.scheme() == "http"
                && url.host_str() == Some("127.0.0.1")
                && url.path() == BRIDGE_PATH
        })
}

pub(super) fn configure(
    document: &mut Value,
    base_url: &str,
    key: &str,
    models: &[&str],
) -> Result<bool, String> {
    let mapping = document
        .as_mapping_mut()
        .ok_or_else(|| "Core configuration must be a mapping".to_string())?;
    let section = Value::String(SECTION.to_string());
    let previous = mapping.get(&section);
    let mut entries = match previous {
        Some(Value::Sequence(entries)) => entries.clone(),
        Some(Value::Null) | None => Vec::new(),
        _ => return Err(format!("Core {SECTION} configuration must be a list")),
    };
    let custom_models: Vec<_> = entries
        .iter()
        .filter(|entry| managed(entry))
        .filter_map(|entry| entry.get("models").and_then(Value::as_sequence))
        .flatten()
        .filter(|route| {
            let Some(model) = route.get("name").and_then(Value::as_str) else {
                return false;
            };
            models.contains(&model)
                && route
                    .get("alias")
                    .and_then(Value::as_str)
                    .is_some_and(|alias| alias != format!("{MODEL_PREFIX}{model}"))
        })
        .map(|route| json!(route))
        .collect();
    entries.retain(|entry| !managed(entry));
    if !models.is_empty() {
        // Explicit aliases keep these names apart from native Claude models.
        let models: Vec<_> = models
            .iter()
            .map(|model| json!({"name": model, "alias": format!("{MODEL_PREFIX}{model}")}))
            .chain(custom_models)
            .collect();
        // A subscription limit or a missing sign-in fails the same way on a
        // retry, and each attempt starts a Claude Code process.
        let entry = json!({"base-url": base_url, "api-key": key, "proxy-url": "direct",
            "request-retry": 0, "cloak": {"mode": "never"}, "models": models});
        entries.push(
            serde_norway::to_value(entry)
                .map_err(|e| format!("Could not configure Claude CLI routes: {e}"))?,
        );
    }
    let next = Value::Sequence(entries);
    if previous == Some(&next) || (previous.is_none() && models.is_empty()) {
        return Ok(false);
    }
    mapping.insert(section, next);
    Ok(true)
}
