use super::{Endpoint, Model, BRIDGE_PATH};
use serde_json::json;
use serde_norway::Value;
use std::{fs, path::Path};

pub(super) fn write(
    path: &Path,
    base_url: &str,
    key: &str,
    models: &[Model],
) -> Result<(), String> {
    let _guard = crate::lock_core_config_file()?;
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Could not read Copilot routing configuration: {error}"
            ))
        }
    };
    #[cfg(unix)]
    if !models.is_empty() {
        use std::os::unix::fs::PermissionsExt;
        let directory = path
            .parent()
            .ok_or_else(|| "Copilot routing configuration has no directory".to_string())?;
        // The adapter key is in config.yaml. Protect the directory as well as
        // the file, since the core and GUI both replace that file during edits.
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("Could not protect the Copilot routing directory: {error}"))?;
    }
    if let Some(updated) = crate::patch_core_yaml_document(&content, |document| {
        configure(document, base_url, key, models)
    })? {
        crate::write_yaml_if_changed(path, &updated)?;
    }
    Ok(())
}

// Ownership is scoped to our loopback path, not a user-visible provider name or
// model prefix. Native credentials and user-created Copilot API entries survive.
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
    models: &[Model],
) -> Result<bool, String> {
    let mapping = document
        .as_mapping_mut()
        .ok_or_else(|| "Core configuration must be a mapping".to_string())?;
    let mut changed = false;
    for (section, endpoint) in [
        ("openai-compatibility", Endpoint::Chat),
        ("codex-api-key", Endpoint::Responses),
        ("claude-api-key", Endpoint::Messages),
    ] {
        let section_key = Value::String(section.to_string());
        let previous = mapping.get(&section_key);
        let mut entries = match previous {
            Some(Value::Sequence(entries)) => entries.clone(),
            Some(Value::Null) | None => Vec::new(),
            _ => return Err(format!("Core {section} configuration must be a list")),
        };
        entries.retain(|entry| !managed(entry));
        let selected: Vec<_> = models
            .iter()
            .filter(|m| m.endpoint == endpoint)
            .map(|m| json!({"name": m.id, "alias": format!("copilot/{}", m.id)}))
            .collect();
        if !selected.is_empty() {
            // Explicit aliases (rather than prefix) prevent the core from also
            // exposing unprefixed names when force-model-prefix is disabled.
            let entry = match endpoint {
                Endpoint::Chat => json!({"name": "GitHub Copilot", "base-url": base_url,
                    "api-key-entries": [{"api-key": key, "proxy-url": "direct"}], "models": selected}),
                Endpoint::Responses => json!({"base-url": base_url, "api-key": key,
                    "proxy-url": "direct", "models": selected}),
                Endpoint::Messages => json!({"base-url": base_url, "api-key": key,
                    "proxy-url": "direct", "cloak": {"mode": "never"}, "models": selected}),
            };
            entries.push(
                serde_norway::to_value(entry)
                    .map_err(|e| format!("Could not configure Copilot routes: {e}"))?,
            );
        }
        let next = Value::Sequence(entries);
        if previous != Some(&next)
            && (previous.is_some() || next.as_sequence().is_some_and(|v| !v.is_empty()))
        {
            mapping.insert(section_key, next);
            changed = true;
        }
    }
    Ok(changed)
}
