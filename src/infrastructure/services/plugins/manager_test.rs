//! Manager-level tests for the admin management surface (install / toggle /
//! remove) and disabled-state persistence. These drive a real Extism sandbox,
//! so they run only under `cargo test --features plugins`.
//!
//! The `.wasm` fixtures are the same ones the runtime tests use, built by
//! `scripts/build-plugin-hello.sh`.

use super::ExtismPluginManager;
use crate::application::ports::plugin_ports::{PluginDispatchPort, PluginManagementPort};
use crate::common::config::PluginConfig;

fn cfg() -> PluginConfig {
    PluginConfig::default()
}

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

/// A valid manifest for the `hello.wasm` fixture (subscribes to both events).
fn hello_manifest() -> String {
    r#"
[plugin]
id = "com.example.hello"
name = "Hello"
version = "0.1.0"
abi = 0
entrypoint = "hello.wasm"

[events]
subscribe = ["file.uploaded", "user.login"]
"#
    .to_string()
}

/// A manifest that parses fine but points at the `wrong_abi.wasm` fixture,
/// which reports ABI 1 at runtime.
fn wrong_abi_manifest() -> String {
    r#"
[plugin]
id = "com.example.wrongabi"
name = "Wrong ABI"
version = "0.1.0"
abi = 0
entrypoint = "wrong_abi.wasm"

[events]
subscribe = ["file.uploaded"]
"#
    .to_string()
}

#[test]
fn install_loads_plugin_and_writes_files() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    assert_eq!(mgr.loaded_count(), 0);

    let info = mgr
        .install(&hello_manifest(), fixture("hello.wasm"))
        .expect("install should succeed");

    assert_eq!(info.id, "com.example.hello");
    assert_eq!(info.name, "Hello");
    assert!(info.enabled);
    assert_eq!(info.subscriptions, vec!["file.uploaded", "user.login"]);
    assert_eq!(mgr.loaded_count(), 1);

    let plugin_dir = tmp.path().join("com.example.hello");
    assert!(plugin_dir.join("plugin.toml").exists());
    assert!(plugin_dir.join("hello.wasm").exists());

    // The live dispatch path sees it immediately.
    assert!(mgr.has_subscribers("file.uploaded"));
}

/// Build an in-memory `.zip` with the given entries (name, bytes).
fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(*name, SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn install_bundle_from_zip_loads_plugin() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());

    // Wrap everything under a top-level folder to exercise prefix resolution.
    let zip = make_zip(&[
        ("hello/plugin.toml", hello_manifest().as_bytes()),
        ("hello/hello.wasm", &fixture("hello.wasm")),
    ]);

    let info = mgr
        .install_bundle(zip)
        .expect("bundle install should succeed");
    assert_eq!(info.id, "com.example.hello");
    assert!(info.enabled);
    assert_eq!(mgr.loaded_count(), 1);
    assert!(
        tmp.path()
            .join("com.example.hello")
            .join("hello.wasm")
            .exists()
    );
}

#[test]
fn install_bundle_without_manifest_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());

    let zip = make_zip(&[("hello.wasm", &fixture("hello.wasm"))]);
    let err = mgr
        .install_bundle(zip)
        .expect_err("a zip without plugin.toml must be rejected");
    assert_eq!(err.reason(), "no_manifest_in_zip");
    assert_eq!(mgr.loaded_count(), 0);
}

#[test]
fn install_bundle_missing_entrypoint_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());

    // Manifest declares entrypoint = "hello.wasm", but the zip omits it.
    let zip = make_zip(&[("plugin.toml", hello_manifest().as_bytes())]);
    let err = mgr
        .install_bundle(zip)
        .expect_err("a zip missing the entrypoint wasm must be rejected");
    assert_eq!(err.reason(), "entrypoint_not_in_zip");
    assert_eq!(mgr.loaded_count(), 0);
}

#[test]
fn install_bundle_oversized_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    // Tiny decompressed ceiling so the ~130 KiB wasm fixture trips it cheaply.
    let mut config = cfg();
    config.max_bundle_decompressed_bytes = 1024;
    let mgr = ExtismPluginManager::load_from_dir(config, tmp.path());

    let zip = make_zip(&[
        ("plugin.toml", hello_manifest().as_bytes()),
        ("hello.wasm", &fixture("hello.wasm")),
    ]);
    let err = mgr
        .install_bundle(zip)
        .expect_err("a bundle over the decompressed ceiling must be rejected");
    assert_eq!(err.reason(), "too_large");
    assert_eq!(mgr.loaded_count(), 0);
}

#[test]
fn install_bundle_with_garbage_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    let err = mgr
        .install_bundle(b"not a zip file".to_vec())
        .expect_err("non-zip bytes must be rejected");
    assert_eq!(err.reason(), "bad_zip");
}

#[test]
fn install_duplicate_id_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    mgr.install(&hello_manifest(), fixture("hello.wasm"))
        .unwrap();

    let err = mgr
        .install(&hello_manifest(), fixture("hello.wasm"))
        .expect_err("second install of the same id must fail");
    assert_eq!(err.reason(), "id_exists");
    assert_eq!(mgr.loaded_count(), 1);
}

#[test]
fn install_wrong_abi_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());

    let err = mgr
        .install(&wrong_abi_manifest(), fixture("wrong_abi.wasm"))
        .expect_err("a plugin reporting the wrong ABI must be rejected");
    assert_eq!(err.reason(), "abi_mismatch");
    assert_eq!(mgr.loaded_count(), 0);
    // Nothing should have been written to disk.
    assert!(!tmp.path().join("com.example.wrongabi").exists());
}

#[test]
fn disable_stops_dispatch_and_persists_across_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    mgr.install(&hello_manifest(), fixture("hello.wasm"))
        .unwrap();
    assert!(mgr.has_subscribers("file.uploaded"));

    mgr.set_enabled("com.example.hello", false).unwrap();
    assert!(!mgr.has_subscribers("file.uploaded"));
    assert!(
        tmp.path()
            .join("com.example.hello")
            .join(".disabled")
            .exists()
    );

    // A fresh manager re-reads the marker and loads it disabled.
    drop(mgr);
    let reloaded = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    assert_eq!(reloaded.loaded_count(), 1);
    let info = reloaded.list();
    assert_eq!(info.len(), 1);
    assert!(!info[0].enabled);
    assert!(!reloaded.has_subscribers("file.uploaded"));

    // Re-enabling removes the marker.
    reloaded.set_enabled("com.example.hello", true).unwrap();
    assert!(reloaded.has_subscribers("file.uploaded"));
    assert!(
        !tmp.path()
            .join("com.example.hello")
            .join(".disabled")
            .exists()
    );
}

#[test]
fn set_enabled_unknown_id_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    let err = mgr.set_enabled("does.not.exist", false).unwrap_err();
    assert_eq!(err.reason(), "not_found");
}

#[test]
fn remove_unloads_and_deletes_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    mgr.install(&hello_manifest(), fixture("hello.wasm"))
        .unwrap();
    let plugin_dir = tmp.path().join("com.example.hello");
    assert!(plugin_dir.exists());

    mgr.remove("com.example.hello").unwrap();
    assert_eq!(mgr.loaded_count(), 0);
    assert!(!plugin_dir.exists());

    let err = mgr.remove("com.example.hello").unwrap_err();
    assert_eq!(err.reason(), "not_found");
}

/// Regression guard: dispatch must persist a log entry for *every* invocation,
/// including a successful one (it previously only logged failures). We dispatch
/// a `file.uploaded` event and then poll the plugin's structured log until an
/// `outcome` row appears.
#[tokio::test(flavor = "multi_thread")]
async fn dispatch_writes_outcome_row_on_success() {
    use crate::application::ports::plugin_ports::{EVENT_FILE_UPLOADED, LogQuery, PluginEvent};

    let tmp = tempfile::tempdir().unwrap();
    let mgr = ExtismPluginManager::load_from_dir(cfg(), tmp.path());
    mgr.install(&hello_manifest(), fixture("hello.wasm"))
        .unwrap();

    mgr.dispatch(PluginEvent {
        name: EVENT_FILE_UPLOADED,
        user_id: None,
        invocation_id: "test-invocation".to_string(),
        payload: serde_json::json!({ "path": "/x.txt", "size": 1, "mime": "text/plain" }),
    });

    // dispatch is fire-and-forget on the blocking pool, and the log write is an
    // ordered async hand-off; poll until the outcome row is durable.
    let mut found = false;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let page = mgr
            .read_logs(
                "com.example.hello",
                LogQuery {
                    level: None,
                    search: None,
                    offset: 0,
                    limit: 100,
                },
            )
            .await
            .unwrap();
        if page.entries.iter().any(|e| e.kind == "outcome") {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "dispatch should persist an outcome row for the invocation"
    );
}
