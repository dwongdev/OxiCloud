//! Plugin-runtime acceptance + failure-isolation tests, plus manifest-validation
//! unit tests.
//!
//! The `.wasm` fixtures are built and committed by `scripts/build-plugin-hello.sh`
//! from `wasm/oxicloud-plugin-hello/`. Run with `cargo test --features plugins`.

use std::time::{Duration, Instant};

use super::ExtismPluginManager;
use super::manifest;
use super::runtime::{InvokeOutcome, PluginRuntime};
use crate::application::ports::plugin_ports::event_export_name;
use crate::common::config::PluginConfig;

fn cfg() -> PluginConfig {
    PluginConfig::default()
}

/// Load a committed `.wasm` fixture, failing with a build hint if it's missing.
fn fixture(name: &str) -> Vec<u8> {
    let path = format!(
        "{}/tests/fixtures/plugins/{}",
        env!("CARGO_MANIFEST_DIR"),
        name
    );
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!("missing fixture {path}: {e}\n  run scripts/build-plugin-hello.sh to (re)build it")
    })
}

fn file_uploaded_input() -> String {
    serde_json::json!({
        "abi": 0,
        "event": "file.uploaded",
        "context": {
            "plugin_id": "com.example.hello",
            "user_id": "u_test",
            "invocation_id": "inv_test_0001"
        },
        "payload": { "path": "/photos/2026/cat.jpg", "size": 81234, "mime": "image/jpeg" }
    })
    .to_string()
}

fn user_login_input() -> String {
    serde_json::json!({
        "abi": 0,
        "event": "user.login",
        "context": {
            "plugin_id": "com.example.hello",
            "user_id": "u_test",
            "invocation_id": "inv_login_0001"
        },
        "payload": {
            "user_id": "u_test",
            "username": "alice",
            "email": "alice@example.com",
            "first_login": true,
            "is_external": false
        }
    })
    .to_string()
}

// ---- The M0 exit criterion: the full loop, per event ------------------------

#[test]
fn acceptance_file_uploaded_returns_ok_and_calls_host_log() {
    let rt = PluginRuntime::new("com.example.hello", fixture("hello.wasm"));
    let result = rt.invoke(&cfg(), "on_file_uploaded", "inv", &file_uploaded_input());

    assert!(
        result.outcome.is_ok(),
        "plugin did not complete: {:?}",
        result.outcome
    );
    assert!(
        result.logs.iter().any(|(level, msg)| level == "info"
            && msg.contains("hello plugin saw upload: /photos/2026/cat.jpg")),
        "expected the plugin's host log line, got: {:?}",
        result.logs
    );
}

#[test]
fn acceptance_user_login_returns_ok_and_calls_host_log() {
    let rt = PluginRuntime::new("com.example.hello", fixture("hello.wasm"));
    let result = rt.invoke(&cfg(), "on_user_login", "inv", &user_login_input());

    assert!(
        result.outcome.is_ok(),
        "plugin did not complete: {:?}",
        result.outcome
    );
    assert!(
        result.logs.iter().any(|(level, msg)| level == "info"
            && msg.contains("hello plugin saw login: user u_test (first_login=true)")),
        "expected the plugin's user.login log line, got: {:?}",
        result.logs
    );
}

// ---- The guarantees, not just the happy path --------------------------------

#[test]
fn rejects_wrong_abi() {
    let rt = PluginRuntime::new("com.example.wrong-abi", fixture("wrong_abi.wasm"));
    assert!(
        matches!(
            rt.check_loadable(&cfg(), &[]),
            InvokeOutcome::AbiMismatch { got: 1 }
        ),
        "wrong-abi plugin should be rejected at load"
    );
}

#[test]
fn load_requires_subscribed_event_exports() {
    let cfg = cfg();
    let login_export = vec![event_export_name("user.login")];

    // hello.wasm exports both handlers -> loadable for user.login.
    let hello = PluginRuntime::new("com.example.hello", fixture("hello.wasm"));
    assert!(matches!(
        hello.check_loadable(&cfg, &login_export),
        InvokeOutcome::Ok
    ));

    // omit_login.wasm lacks on_user_login -> rejected when it claims user.login.
    let omit = PluginRuntime::new("com.example.omit", fixture("omit_login.wasm"));
    assert!(
        matches!(
            omit.check_loadable(&cfg, &login_export),
            InvokeOutcome::MissingExport(ref e) if e == "on_user_login"
        ),
        "omit_login must be rejected for a user.login subscription"
    );
    // …but it is fine for file.uploaded, which it does export.
    assert!(matches!(
        omit.check_loadable(&cfg, &[event_export_name("file.uploaded")]),
        InvokeOutcome::Ok
    ));
}

#[test]
fn contains_a_panicking_plugin() {
    let rt = PluginRuntime::new("com.example.panic", fixture("panic.wasm"));
    let result = rt.invoke(&cfg(), "on_file_uploaded", "inv", &file_uploaded_input());
    assert!(
        matches!(result.outcome, InvokeOutcome::Trap(_)),
        "expected a contained trap, got {:?}",
        result.outcome
    );
    // Reaching this line at all proves the host process survived the trap.
}

#[test]
fn enforces_timeout() {
    let rt = PluginRuntime::new("com.example.sleep", fixture("sleep.wasm"));
    let start = Instant::now();
    let result = rt.invoke(&cfg(), "on_file_uploaded", "inv", &file_uploaded_input());
    let elapsed = start.elapsed();

    assert!(
        matches!(result.outcome, InvokeOutcome::Timeout),
        "expected a timeout, got {:?}",
        result.outcome
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "timeout took too long to fire: {elapsed:?}"
    );
}

#[test]
fn idle_eviction_drops_and_recompiles() {
    let rt = PluginRuntime::new("com.example.hello", fixture("hello.wasm"));
    // First invoke compiles + caches the module.
    let r1 = rt.invoke(&cfg(), "on_file_uploaded", "inv1", &file_uploaded_input());
    assert!(r1.outcome.is_ok(), "first invoke: {:?}", r1.outcome);

    // Idle past a zero TTL -> the cached module is dropped.
    assert!(
        rt.evict_if_idle(Duration::ZERO),
        "a just-idle module should be evicted"
    );
    // Nothing left to evict the second time.
    assert!(
        !rt.evict_if_idle(Duration::ZERO),
        "second eviction is a no-op"
    );

    // The next invoke recompiles transparently and still works.
    let r2 = rt.invoke(&cfg(), "on_file_uploaded", "inv2", &file_uploaded_input());
    assert!(
        r2.outcome.is_ok(),
        "recompile after eviction: {:?}",
        r2.outcome
    );
    // A long TTL never evicts a freshly-used module.
    assert!(
        !rt.evict_if_idle(Duration::from_secs(3600)),
        "a fresh module must not be evicted"
    );
}

#[test]
fn no_network() {
    let rt = PluginRuntime::new("com.example.net", fixture("net.wasm"));
    let result = rt.invoke(&cfg(), "on_file_uploaded", "inv", &file_uploaded_input());
    assert!(
        !result.outcome.is_ok(),
        "network access should be denied, got {:?}",
        result.outcome
    );
}

// ---- Manager discovery + dispatch ------------------------------------------

/// Write a one-plugin directory (plugin.toml + the given wasm) under a tempdir
/// and load a manager from it.
fn manager_with(wasm_name: &str, subscribe_toml: &str) -> (tempfile::TempDir, ExtismPluginManager) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("plugin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("plugin.wasm"), fixture(wasm_name)).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        format!(
            r#"
[plugin]
id = "com.example.test"
name = "Test"
version = "0.1.0"
abi = 0
entrypoint = "plugin.wasm"

[events]
subscribe = {subscribe_toml}
"#
        ),
    )
    .unwrap();
    let manager = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    (tmp, manager)
}

#[tokio::test]
async fn manager_loads_and_dispatches_both_events() {
    use crate::application::ports::plugin_ports::{
        EVENT_FILE_UPLOADED, EVENT_USER_LOGIN, PluginDispatchPort, PluginEvent,
    };

    let (_tmp, manager) = manager_with("hello.wasm", r#"["file.uploaded", "user.login"]"#);
    assert_eq!(manager.loaded_count(), 1, "the valid plugin should load");
    assert!(manager.has_subscribers("file.uploaded"));
    assert!(manager.has_subscribers("user.login"));
    assert!(!manager.has_subscribers("file.deleted"));

    // Both dispatches run the plugin on the blocking pool; neither may panic.
    manager.dispatch(PluginEvent {
        name: EVENT_FILE_UPLOADED,
        user_id: Some("u_test".into()),
        invocation_id: "inv_upload".into(),
        payload: serde_json::json!({ "path": "/a.txt", "size": 3, "mime": "text/plain" }),
    });
    manager.dispatch(PluginEvent {
        name: EVENT_USER_LOGIN,
        user_id: Some("u_test".into()),
        invocation_id: "inv_login".into(),
        payload: serde_json::json!({ "user_id": "u_test", "first_login": false }),
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
}

#[test]
fn manager_rejects_plugin_missing_a_subscribed_export() {
    // omit_login.wasm subscribes to user.login but doesn't export on_user_login.
    let (_tmp, rejected) = manager_with("omit_login.wasm", r#"["user.login"]"#);
    assert_eq!(rejected.loaded_count(), 0, "missing export -> not loaded");

    // The same wasm is fine when it only claims an event it actually exports.
    let (_tmp2, loaded) = manager_with("omit_login.wasm", r#"["file.uploaded"]"#);
    assert_eq!(loaded.loaded_count(), 1);
}

// ---- Manifest validation (no wasm needed) -----------------------------------

const VALID_MANIFEST: &str = r#"
[plugin]
id = "com.example.hello"
name = "Hello"
version = "0.1.0"
abi = 0
entrypoint = "hello.wasm"

[events]
subscribe = ["file.uploaded"]
"#;

#[test]
fn manifest_accepts_valid() {
    let m = manifest::parse_and_validate(VALID_MANIFEST).expect("valid manifest");
    assert_eq!(m.plugin.id, "com.example.hello");
}

#[test]
fn manifest_accepts_user_login_and_combined() {
    let login = VALID_MANIFEST.replace(r#"["file.uploaded"]"#, r#"["user.login"]"#);
    assert!(manifest::parse_and_validate(&login).is_ok());

    let both = VALID_MANIFEST.replace(r#"["file.uploaded"]"#, r#"["file.uploaded", "user.login"]"#);
    assert!(manifest::parse_and_validate(&both).is_ok());
}

#[test]
fn manifest_rejects_unknown_field() {
    let toml = format!("{VALID_MANIFEST}\nbogus_top_level = true\n");
    assert_eq!(
        manifest::parse_and_validate(&toml).unwrap_err().reason(),
        "parse_error"
    );
}

#[test]
fn manifest_rejects_abi_mismatch() {
    let toml = VALID_MANIFEST.replace("abi = 0", "abi = 1");
    assert_eq!(
        manifest::parse_and_validate(&toml).unwrap_err().reason(),
        "abi_mismatch"
    );
}

#[test]
fn manifest_rejects_unknown_event() {
    let toml = VALID_MANIFEST.replace(r#"["file.uploaded"]"#, r#"["file.deleted"]"#);
    assert_eq!(
        manifest::parse_and_validate(&toml).unwrap_err().reason(),
        "unknown_event"
    );
}

#[test]
fn manifest_rejects_nonempty_permissions() {
    let toml = format!("{VALID_MANIFEST}\n[permissions]\nfs = \"/tmp\"\n");
    assert_eq!(
        manifest::parse_and_validate(&toml).unwrap_err().reason(),
        "permissions_not_empty"
    );
}
