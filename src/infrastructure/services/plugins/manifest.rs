//! `plugin.toml` parsing + load-time validation (ABI v0).
//!
//! The manifest is the host's source of truth for *what to load and when to
//! call it*. Validation fails closed: unknown sections/keys, a mismatched ABI,
//! an unknown subscribed event, or any non-empty `[permissions]` (M0 grants
//! none) all reject the plugin. A rejected plugin is skipped, never fatal.

use std::collections::BTreeMap;

use crate::application::ports::plugin_ports::{KNOWN_EVENTS, OXICLOUD_PLUGIN_ABI};

/// Parsed `plugin.toml`. `#[serde(deny_unknown_fields)]` on every struct turns
/// stray keys into load errors rather than silently ignored config.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginManifest {
    pub plugin: PluginSection,
    pub events: EventsSection,
    /// M0: must be empty. Any key here rejects the plugin (no grantable
    /// permissions exist yet). Kept as a free map so future keys are *detected*,
    /// not parsed.
    #[serde(default)]
    pub permissions: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSection {
    /// Reverse-DNS, unique per instance.
    pub id: String,
    pub name: String,
    /// The plugin's own semver.
    pub version: String,
    /// Must equal [`OXICLOUD_PLUGIN_ABI`].
    pub abi: u32,
    /// Path to the `.wasm`, relative to the manifest.
    pub entrypoint: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsSection {
    /// Events this plugin wants. Each must be one of `KNOWN_EVENTS`
    /// (`"file.uploaded"`, `"user.login"`); an unknown name rejects the plugin.
    pub subscribe: Vec<String>,
}

/// Why a manifest was rejected. `reason()` yields the stable, machine-readable
/// key used in audit logs.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to parse plugin.toml: {0}")]
    Parse(String),
    #[error("plugin declares ABI {got}, host speaks {want}")]
    AbiMismatch { got: u32, want: u32 },
    #[error("events.subscribe must not be empty")]
    NoEvents,
    #[error("unknown event '{0}' in events.subscribe")]
    UnknownEvent(String),
    #[error("permissions must be empty in ABI v0 (found key '{0}')")]
    PermissionsNotEmpty(String),
}

impl ManifestError {
    /// Stable key for `tracing` audit lines; never reworded across releases.
    pub fn reason(&self) -> &'static str {
        match self {
            ManifestError::Parse(_) => "parse_error",
            ManifestError::AbiMismatch { .. } => "abi_mismatch",
            ManifestError::NoEvents => "no_events",
            ManifestError::UnknownEvent(_) => "unknown_event",
            ManifestError::PermissionsNotEmpty(_) => "permissions_not_empty",
        }
    }
}

/// Parse and validate a `plugin.toml` body. Does not touch the `.wasm`; the
/// caller probes `abi_version` separately after a successful parse.
pub fn parse_and_validate(toml_str: &str) -> Result<PluginManifest, ManifestError> {
    let manifest: PluginManifest =
        toml::from_str(toml_str).map_err(|e| ManifestError::Parse(e.to_string()))?;

    if manifest.plugin.abi != OXICLOUD_PLUGIN_ABI {
        return Err(ManifestError::AbiMismatch {
            got: manifest.plugin.abi,
            want: OXICLOUD_PLUGIN_ABI,
        });
    }

    if manifest.events.subscribe.is_empty() {
        return Err(ManifestError::NoEvents);
    }
    for event in &manifest.events.subscribe {
        if !KNOWN_EVENTS.contains(&event.as_str()) {
            return Err(ManifestError::UnknownEvent(event.clone()));
        }
    }

    if let Some((key, _)) = manifest.permissions.iter().next() {
        return Err(ManifestError::PermissionsNotEmpty(key.clone()));
    }

    Ok(manifest)
}
