use dy_screen_app_lib::app_lifecycle::{InstanceLock, InstanceLockError, ShutdownGate};

#[test]
fn second_instance_is_rejected_until_the_owner_drops_the_lock() {
    let directory = tempfile::tempdir().unwrap();
    let first = InstanceLock::acquire(directory.path()).unwrap();

    assert!(matches!(
        InstanceLock::acquire(directory.path()),
        Err(InstanceLockError::AlreadyRunning)
    ));

    drop(first);
    InstanceLock::acquire(directory.path()).unwrap();
}

#[test]
fn shutdown_can_only_be_claimed_once() {
    let gate = ShutdownGate::default();

    assert!(gate.try_begin());
    assert!(!gate.try_begin());
}

#[test]
fn desktop_dev_script_disables_the_rust_process_watcher() {
    let package_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("package.json");
    let package: serde_json::Value =
        serde_json::from_slice(&std::fs::read(package_path).unwrap()).unwrap();

    assert_eq!(
        package["scripts"]["tauri:dev"],
        serde_json::Value::String("tauri dev --no-watch".to_owned())
    );
}
