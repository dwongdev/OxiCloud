//! The Extism runtime wrapper — a cached compiled module, instantiated fresh per
//! invocation.
//!
//! Isolation is the point: no WASI, no filesystem, no network, a memory cap, and
//! a wall-clock timeout. The only authority a plugin has is the host `log`
//! function. Every boundary crossing is wrapped so a trap/timeout/OOM/malformed
//! output is captured as an [`InvokeOutcome`] and never propagates to the caller.
//!
//! **Compilation is amortized.** A plugin's WASM is compiled once into an
//! [`extism::CompiledPlugin`] and cached; every invocation builds a *fresh*
//! [`extism::Plugin`] instance from it (a new Store/memory → no cross-user
//! state), but pays no recompilation. Per-invocation log attribution rides
//! `call_with_host_context` rather than a baked `UserData`, so the same compiled
//! module serves concurrent invocations without sharing the log buffer. An idle
//! plugin's compiled module is dropped by [`PluginRuntime::evict_if_idle`] to
//! reclaim memory; the next event recompiles (cheaply, from wasmtime's on-disk
//! compilation cache).

use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use extism::{
    CompiledPlugin, CurrentPlugin, Manifest as ExtismManifest, PTR, PluginBuilder, UserData, Val,
    Wasm,
};

use crate::application::ports::plugin_ports::{HOST_NAMESPACE, OXICLOUD_PLUGIN_ABI, PluginOutput};
use crate::common::config::PluginConfig;

/// Per-invocation host context, handed to one `handle` call via
/// `call_with_host_context` and read back by the `log` host function. Each
/// invocation gets its own, so a reused compiled module never mixes two
/// invocations' log lines. `lines` is an `Arc` the caller retains a clone of, to
/// read what the plugin emitted after the call returns.
struct LogSink {
    plugin_id: String,
    invocation_id: String,
    lines: Arc<Mutex<Vec<(String, String)>>>,
}

/// The entire authority surface: log(level, message) -> (). Observe-only — it
/// reads nothing and mutates no host state beyond the per-call sink. Unknown
/// levels clamp to "info". Written without the `host_fn!` macro so it can read
/// the per-invocation [`LogSink`] from the host context.
fn oxi_log(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    _user_data: UserData<()>,
) -> Result<(), extism::Error> {
    let level: String = plugin.memory_get_val(&inputs[0])?;
    let message: String = plugin.memory_get_val(&inputs[1])?;
    let level = match level.as_str() {
        "debug" | "info" | "warn" | "error" => level,
        _ => "info".to_string(),
    };
    let ctx = plugin.host_context::<LogSink>()?;
    // The message is a structured field, never interpolated into the format
    // string — a plugin can't inject newlines into the operational log stream.
    tracing::info!(
        target: "oxicloud::plugins",
        plugin_id = %ctx.plugin_id,
        invocation_id = %ctx.invocation_id,
        plugin_level = %level,
        plugin_message = %message,
        "plugin log"
    );
    ctx.lines
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((level, message));
    Ok(())
}

/// The result of one boundary crossing. Only `Ok` is a success; every other
/// variant is a contained failure the host audit-logs and moves past.
#[derive(Debug)]
pub enum InvokeOutcome {
    /// `handle` returned `{"ok": true}`.
    Ok,
    /// `handle` returned `{"ok": false, "error": ...}`.
    PluginError(String),
    /// A wasm trap (panic/`unreachable`/OOM/etc.).
    Trap(String),
    /// The wall-clock timeout cancelled the call.
    Timeout,
    /// The instance could not be built (bad/unloadable wasm, unresolved import).
    LoadError(String),
    /// `abi_version` returned a value the host does not speak.
    AbiMismatch { got: u32 },
    /// A subscribed event has no matching `on_<event>` export in the module.
    MissingExport(String),
    /// The event handler returned bytes that are not a valid `PluginOutput`.
    MalformedOutput(String),
    /// The serialized input exceeded the configured cap; nothing was invoked.
    MalformedInput { size: usize, max: usize },
}

impl InvokeOutcome {
    pub fn is_ok(&self) -> bool {
        matches!(self, InvokeOutcome::Ok)
    }

    /// The `(level, message)` to record for this outcome in a plugin's log file.
    /// `Ok` is an `info` "completed"; every contained failure is a `warn`/`error`
    /// carrying its detail. The stable machine key is [`InvokeOutcome::reason`].
    pub fn log_detail(&self) -> (&'static str, String) {
        match self {
            InvokeOutcome::Ok => ("info", "invocation completed".to_string()),
            InvokeOutcome::PluginError(e) => ("warn", e.clone()),
            InvokeOutcome::Trap(e) => ("error", e.clone()),
            InvokeOutcome::Timeout => ("error", "wall-clock timeout".to_string()),
            InvokeOutcome::LoadError(e) => ("error", e.clone()),
            InvokeOutcome::AbiMismatch { got } => {
                ("error", format!("abi mismatch: plugin reported {got}"))
            }
            InvokeOutcome::MissingExport(s) => ("error", format!("missing export: {s}")),
            InvokeOutcome::MalformedOutput(e) => ("warn", e.clone()),
            InvokeOutcome::MalformedInput { size, max } => {
                ("warn", format!("input too large: {size} bytes (max {max})"))
            }
        }
    }

    /// Stable, machine-readable key for audit logs.
    pub fn reason(&self) -> &'static str {
        match self {
            InvokeOutcome::Ok => "ok",
            InvokeOutcome::PluginError(_) => "plugin_error",
            InvokeOutcome::Trap(_) => "trap",
            InvokeOutcome::Timeout => "timeout",
            InvokeOutcome::LoadError(_) => "load_error",
            InvokeOutcome::AbiMismatch { .. } => "abi_mismatch",
            InvokeOutcome::MissingExport(_) => "missing_export",
            InvokeOutcome::MalformedOutput(_) => "malformed_output",
            InvokeOutcome::MalformedInput { .. } => "malformed_input",
        }
    }
}

/// Outcome plus whatever the plugin logged (for tests and tracing).
pub struct InvokeResult {
    pub outcome: InvokeOutcome,
    pub logs: Vec<(String, String)>,
}

/// A loaded plugin: the wasm bytes plus a lazily-built, idle-evictable compiled
/// module. A fresh *instance* is built for every invocation (no reuse → no
/// cross-user state); only the *compilation* is shared.
pub struct PluginRuntime {
    plugin_id: String,
    wasm_bytes: Vec<u8>,
    /// The cached compiled module, `None` until first use or after idle
    /// eviction. Guarded by an `RwLock`: invocations take the read lock to
    /// instantiate concurrently; (re)compilation and eviction take the write
    /// lock.
    compiled: RwLock<Option<CompiledPlugin>>,
    /// Last time an instance was built, for idle eviction. Separate lock so it
    /// can be stamped while only holding `compiled` for read.
    last_used: Mutex<Instant>,
}

impl PluginRuntime {
    pub fn new(plugin_id: impl Into<String>, wasm_bytes: Vec<u8>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            wasm_bytes,
            compiled: RwLock::new(None),
            last_used: Mutex::new(Instant::now()),
        }
    }

    /// Compile the WASM into a reusable [`CompiledPlugin`], wiring the sandbox
    /// limits and the sole host import. wasmtime's on-disk cache (extism's
    /// default) makes a repeat compile after eviction cheap.
    fn compile(&self, cfg: &PluginConfig) -> Result<CompiledPlugin, extism::Error> {
        let manifest = ExtismManifest::new([Wasm::data(self.wasm_bytes.clone())])
            .with_memory_max(cfg.max_memory_pages) // pages × 64 KiB
            .with_timeout(Duration::from_millis(cfg.invocation_timeout_ms))
            .disallow_all_hosts(); // no outbound network
        // No allowed_paths -> no filesystem. with_wasi(false) -> no ambient authority.
        PluginBuilder::new(manifest)
            .with_wasi(false)
            .with_function_in_namespace(
                HOST_NAMESPACE,
                "log",
                [PTR, PTR],
                [],
                UserData::new(()),
                oxi_log,
            )
            .compile()
    }

    /// Build a fresh instance from the (cached, lazily-compiled) module. Stamps
    /// `last_used` so the idle sweep leaves an actively-used plugin alone.
    fn instantiate(&self, cfg: &PluginConfig) -> Result<extism::Plugin, InvokeOutcome> {
        // Fast path: already compiled.
        {
            let guard = self.compiled.read().unwrap_or_else(|e| e.into_inner());
            if let Some(compiled) = guard.as_ref() {
                *self.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
                return extism::Plugin::new_from_compiled(compiled)
                    .map_err(|e| InvokeOutcome::LoadError(e.to_string()));
            }
        }
        // Slow path: compile under the write lock (double-checked).
        let mut guard = self.compiled.write().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            match self.compile(cfg) {
                Ok(c) => *guard = Some(c),
                Err(e) => return Err(InvokeOutcome::LoadError(e.to_string())),
            }
        }
        let compiled = guard.as_ref().expect("compiled present after compile");
        *self.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        extism::Plugin::new_from_compiled(compiled)
            .map_err(|e| InvokeOutcome::LoadError(e.to_string()))
    }

    /// Drop the cached compiled module if it hasn't been used within `ttl`,
    /// reclaiming its memory. Returns whether anything was evicted. The next
    /// invocation recompiles transparently.
    pub fn evict_if_idle(&self, ttl: Duration) -> bool {
        let idle = self
            .last_used
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed()
            >= ttl;
        if !idle {
            return false;
        }
        let mut guard = self.compiled.write().unwrap_or_else(|e| e.into_inner());
        guard.take().is_some()
    }

    /// Probe loadability: compile (caching the module), check `abi_version`,
    /// then verify every `required_export` (the `on_<event>` symbol for each
    /// subscribed event) exists. Rejects lying, unloadable, or
    /// incompletely-implemented plugins before they are ever registered.
    pub fn check_loadable(&self, cfg: &PluginConfig, required_exports: &[String]) -> InvokeOutcome {
        let mut plugin = match self.instantiate(cfg) {
            Ok(p) => p,
            Err(o) => return o,
        };
        match plugin.call::<(), u32>("abi_version", ()) {
            Ok(v) if v == OXICLOUD_PLUGIN_ABI => {}
            Ok(v) => return InvokeOutcome::AbiMismatch { got: v },
            Err(e) => return classify_call_error(e),
        }
        for export in required_exports {
            if !plugin.function_exists(export) {
                return InvokeOutcome::MissingExport(export.clone());
            }
        }
        InvokeOutcome::Ok
    }

    /// Run one event-handler invocation, fully fault-isolated. `export` is the
    /// `on_<event>` symbol to call (see `event_export_name`).
    pub fn invoke(
        &self,
        cfg: &PluginConfig,
        export: &str,
        invocation_id: &str,
        input_json: &str,
    ) -> InvokeResult {
        if input_json.len() > cfg.max_input_bytes {
            return InvokeResult {
                outcome: InvokeOutcome::MalformedInput {
                    size: input_json.len(),
                    max: cfg.max_input_bytes,
                },
                logs: Vec::new(),
            };
        }

        let lines = Arc::new(Mutex::new(Vec::new()));
        let drain = || lines.lock().unwrap_or_else(|e| e.into_inner()).clone();

        let mut plugin = match self.instantiate(cfg) {
            Ok(p) => p,
            Err(outcome) => {
                return InvokeResult {
                    outcome,
                    logs: drain(),
                };
            }
        };

        // Version negotiation at the door (cheap; no recompile).
        match plugin.call::<(), u32>("abi_version", ()) {
            Ok(v) if v == OXICLOUD_PLUGIN_ABI => {}
            Ok(v) => {
                return InvokeResult {
                    outcome: InvokeOutcome::AbiMismatch { got: v },
                    logs: drain(),
                };
            }
            Err(e) => {
                return InvokeResult {
                    outcome: classify_call_error(e),
                    logs: drain(),
                };
            }
        }

        let sink = LogSink {
            plugin_id: self.plugin_id.clone(),
            invocation_id: invocation_id.to_string(),
            lines: lines.clone(),
        };

        // The actual call. Traps, timeouts, and OOM all surface here as Err.
        let outcome = match plugin
            .call_with_host_context::<&str, String, LogSink>(export, input_json, sink)
        {
            Ok(out) => match serde_json::from_str::<PluginOutput>(&out) {
                Ok(parsed) if parsed.ok => InvokeOutcome::Ok,
                Ok(parsed) => {
                    InvokeOutcome::PluginError(parsed.error.unwrap_or_else(|| "unspecified".into()))
                }
                Err(e) => InvokeOutcome::MalformedOutput(e.to_string()),
            },
            Err(e) => classify_call_error(e),
        };

        InvokeResult {
            outcome,
            logs: drain(),
        }
        // `plugin` (instance) dropped here -> sandbox memory reclaimed. The
        // compiled module stays cached for the next invocation.
    }
}

/// Extism signals a wall-clock timeout with `Error::msg("timeout")`; everything
/// else from a `call` is a trap (panic, `unreachable`, OOM, etc.).
fn classify_call_error(e: extism::Error) -> InvokeOutcome {
    let msg = e.to_string();
    if msg.to_ascii_lowercase().contains("timeout") {
        InvokeOutcome::Timeout
    } else {
        InvokeOutcome::Trap(msg)
    }
}
