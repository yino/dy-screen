#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;

use dy_screen::asr::{AsrResourceResolver, ResourcePreflight, SystemResourceProbe};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

struct FixedProbe {
    total: u64,
    available: u64,
    disk: u64,
}

impl SystemResourceProbe for FixedProbe {
    fn total_memory_bytes(&self) -> Option<u64> {
        Some(self.total)
    }

    fn available_memory_bytes(&self) -> Option<u64> {
        Some(self.available)
    }

    fn available_disk_bytes(&self, _path: &Path) -> Option<u64> {
        Some(self.disk)
    }
}

fn write_executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn integrity(root: &Path, file: &str) -> serde_json::Value {
    let bytes = std::fs::read(root.join(file)).unwrap();
    serde_json::json!({
        "file": file,
        "sizeBytes": bytes.len(),
        "sha256": hex::encode(Sha256::digest(bytes))
    })
}

fn manifest(root: &Path, model: &[u8], vad: &[u8], normalization: &[u8]) -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        std::env::consts::OS
    };
    let arch = std::env::consts::ARCH;
    serde_json::json!({
        "schemaVersion": 1,
        "bundleVersion": "test",
        "engine": {
            "id": "whisper.cpp",
            "version": "v1.9.1",
            "sourceCommit": "test"
        },
        "model": {
            "logicalId": "small-test",
            "version": "1",
            "file": "models/model.bin",
            "sizeBytes": model.len(),
            "sha256": hex::encode(Sha256::digest(model)),
            "source": "test",
            "license": "test"
        },
        "vad": {
            "logicalId": "vad-test",
            "version": "1",
            "file": "models/vad.bin",
            "sizeBytes": vad.len(),
            "sha256": hex::encode(Sha256::digest(vad)),
            "source": "test",
            "license": "test"
        },
        "normalization": {
            "logicalId": "normalization-test",
            "version": "1",
            "file": "normalization/TSCharacters.txt",
            "sizeBytes": normalization.len(),
            "sha256": hex::encode(Sha256::digest(normalization)),
            "source": "test",
            "license": "test"
        },
        "platforms": [{
            "os": os,
            "arch": arch,
            "accelerator": "cpu",
            "sidecar": "bin/whisper-cli",
            "vadSidecar": "bin/vad-sidecar",
            "ffmpeg": "bin/ffmpeg",
            "ffprobe": "bin/ffprobe",
            "minimumMemoryBytes": 8589934592_u64,
            "minimumFreeDiskBytes": 2147483648_u64,
            "maximumThreads": 4,
            "resourceIntegrity": [
                integrity(root, "bin/whisper-cli"),
                integrity(root, "bin/vad-sidecar"),
                integrity(root, "bin/ffmpeg"),
                integrity(root, "bin/ffprobe")
            ]
        }]
    })
    .to_string()
}

fn resource_root() -> (tempfile::TempDir, String) {
    let directory = tempdir().unwrap();
    let root = directory.path();
    let version_script = "#!/bin/sh\nprintf '%s\\n' 'whisper.cpp version: 1.9.1'\n";
    write_executable(&root.join("bin/whisper-cli"), version_script);
    write_executable(&root.join("bin/vad-sidecar"), "#!/bin/sh\nexit 0\n");
    write_executable(&root.join("bin/ffmpeg"), "#!/bin/sh\nexit 0\n");
    write_executable(&root.join("bin/ffprobe"), "#!/bin/sh\nexit 0\n");
    std::fs::create_dir_all(root.join("models")).unwrap();
    std::fs::create_dir_all(root.join("normalization")).unwrap();
    let model = b"model fixture";
    let vad = b"vad fixture";
    let normalization = "歡 欢\n".as_bytes();
    std::fs::write(root.join("models/model.bin"), model).unwrap();
    std::fs::write(root.join("models/vad.bin"), vad).unwrap();
    std::fs::write(root.join("normalization/TSCharacters.txt"), normalization).unwrap();
    let manifest = manifest(root, model, vad, normalization);
    (directory, manifest)
}

#[test]
fn resolver_and_preflight_validate_platform_hash_permissions_memory_and_disk() {
    let (directory, manifest) = resource_root();
    let mut unsealed: serde_json::Value = serde_json::from_str(&manifest).unwrap();
    unsealed["platforms"][0]["resourceIntegrity"] = serde_json::json!([]);
    assert_eq!(
        AsrResourceResolver::resolve(directory.path(), &unsealed.to_string())
            .unwrap_err()
            .code,
        "invalid_resource_manifest"
    );

    let resources = AsrResourceResolver::resolve(directory.path(), &manifest).unwrap();
    let preflight = ResourcePreflight::new(Arc::new(FixedProbe {
        total: 8 * 1024 * 1024 * 1024,
        available: 2 * 1024 * 1024 * 1024,
        disk: 4 * 1024 * 1024 * 1024,
    }));
    let report = preflight.diagnose(&resources).unwrap();
    assert!(report.ready, "{:?}", report.checks);
    assert_eq!(report.identity.model_id, "small-test");

    std::fs::write(&resources.ffmpeg, b"corrupt").unwrap();
    let report = preflight.diagnose(&resources).unwrap();
    assert!(!report.ready);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "ffmpeg" && !check.passed)
    );

    std::fs::write(&resources.model, b"corrupt").unwrap();
    let report = preflight.diagnose(&resources).unwrap();
    assert!(!report.ready);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "asr_model" && !check.passed)
    );
}

#[test]
fn eight_gigabyte_preflight_rejects_low_total_or_available_memory() {
    let (directory, manifest) = resource_root();
    let resources = AsrResourceResolver::resolve(directory.path(), &manifest).unwrap();
    let preflight = ResourcePreflight::new(Arc::new(FixedProbe {
        total: 4 * 1024 * 1024 * 1024,
        available: 128 * 1024 * 1024,
        disk: 4 * 1024 * 1024 * 1024,
    }));
    let report = preflight.diagnose(&resources).unwrap();
    assert!(!report.ready);
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "total_memory" && !check.passed)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|check| check.code == "available_memory" && !check.passed)
    );
}
