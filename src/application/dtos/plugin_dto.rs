//! DTOs for the admin plugin-management API.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use crate::application::ports::plugin_ports::{LogEntry, LogPage, PluginInfo, RetentionSettings};

/// A single installed plugin as returned by `GET /api/admin/plugins`.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PluginInfoDto {
    pub id: String,
    pub name: String,
    pub version: String,
    pub abi: u32,
    /// Events the plugin subscribes to (e.g. `file.uploaded`).
    pub subscriptions: Vec<String>,
    pub enabled: bool,
}

impl From<PluginInfo> for PluginInfoDto {
    fn from(p: PluginInfo) -> Self {
        Self {
            id: p.id,
            name: p.name,
            version: p.version,
            abi: p.abi,
            subscriptions: p.subscriptions,
            enabled: p.enabled,
        }
    }
}

/// Request body for `PUT /api/admin/plugins/{id}/enabled`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetEnabledDto {
    pub enabled: bool,
}

/// A single structured log entry as returned by the admin log viewer / stream.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PluginLogEntryDto {
    /// RFC 3339 timestamp.
    pub ts: String,
    pub invocation_id: String,
    /// `"plugin"` (plugin-emitted line) or `"outcome"` (host invocation result).
    pub kind: String,
    /// `debug` | `info` | `warn` | `error`.
    pub level: String,
    /// Stable outcome key for `kind = "outcome"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub msg: String,
}

impl From<LogEntry> for PluginLogEntryDto {
    fn from(e: LogEntry) -> Self {
        Self {
            ts: e.ts,
            invocation_id: e.invocation_id,
            kind: e.kind,
            level: e.level,
            reason: e.reason,
            msg: e.msg,
        }
    }
}

/// One page of log entries, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PluginLogPageDto {
    pub entries: Vec<PluginLogEntryDto>,
    /// Total entries matching the filter (across all pages).
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

impl PluginLogPageDto {
    pub fn from_page(page: LogPage, limit: usize, offset: usize) -> Self {
        Self {
            entries: page
                .entries
                .into_iter()
                .map(PluginLogEntryDto::from)
                .collect(),
            total: page.total,
            limit,
            offset,
        }
    }
}

/// Query string for `GET /api/admin/plugins/{id}/logs`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct PluginLogQueryDto {
    /// Keep only entries at this level (`debug`/`info`/`warn`/`error`).
    pub level: Option<String>,
    /// Case-insensitive substring filter on the message.
    pub search: Option<String>,
    /// Max entries to return (clamped server-side).
    pub limit: Option<usize>,
    /// Newest-first entries to skip.
    pub offset: Option<usize>,
}

/// Per-plugin retention policy (request + response body).
///
/// Both limits are accepted as-is, including `0`, which means "purge all rotated
/// segments on the next sweep" (the active log file is never touched). This is
/// intentional — an operator can deliberately keep nothing.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct PluginRetentionDto {
    /// Delete rotated segments older than this many days. `0` = keep none.
    pub retention_days: u32,
    /// Aggregate byte ceiling on kept segments for the plugin. `0` = keep none.
    pub max_bytes: u64,
}

impl From<RetentionSettings> for PluginRetentionDto {
    fn from(s: RetentionSettings) -> Self {
        Self {
            retention_days: s.retention_days,
            max_bytes: s.max_bytes,
        }
    }
}

impl From<PluginRetentionDto> for RetentionSettings {
    fn from(d: PluginRetentionDto) -> Self {
        Self {
            retention_days: d.retention_days,
            max_bytes: d.max_bytes,
        }
    }
}
