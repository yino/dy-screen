//! 客户端数据边界的单一登记处（步骤 2 任务 10.1 / 10.3）。
//!
//! v2 把「全不上传」改成「两个且仅两个显式例外」，并把例外收敛到**一处**登记，
//! 而不是让每个能力各自开洞。本模块是那份登记：
//!
//! 1. `cloud_asr_audio`：经用户显式授权的**临时音频**直传百炼（云端转写）；
//! 2. `llm_minimal_text`：经用户显式授权的**最小文本**发往百炼（模型调用）。
//!
//! 两条都要求用户显式授权，且都由 Rust 侧发起——WebView 不接触凭据，也不发起上传。
//! 其余能力（录制、视频库浏览、预览、封面、本地 ASR、转场素材目录同步、激活与心跳）
//! MUST NOT 上传任何用户内容。
//!
//! `allowed_modules` 不是装饰：`data_boundary` 测试会扫描上传原语（`multipart`、
//! `Part::bytes`、`Part::file`）的出现位置，任何落在登记之外的命中都会让测试失败。

/// 一条用户内容上传例外。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserContentUploadException {
    /// 稳定机器标识，供审计与诊断引用。
    pub key: &'static str,
    /// 面向评审的中文说明。
    pub description: &'static str,
    /// 是否要求用户显式授权（两条都必须是 `true`）。
    pub requires_user_authorization: bool,
    /// 允许承载该上传的源码路径（仓库相对路径）。
    pub allowed_modules: &'static [&'static str],
}

/// 用户内容上传例外的**完整集合**：只有两条。
pub const USER_CONTENT_UPLOAD_EXCEPTIONS: [UserContentUploadException; 2] = [
    UserContentUploadException {
        key: "cloud_asr_audio",
        description: "经用户显式授权的临时音频直传百炼（云端转写）",
        requires_user_authorization: true,
        allowed_modules: &["src/asr/cloud.rs"],
    },
    UserContentUploadException {
        key: "llm_minimal_text",
        description: "经用户显式授权的最小文本发往百炼（模型调用）",
        requires_user_authorization: true,
        // LLM 调用发送的是 JSON 文本体，不含任何文件体，因此这里登记的是文本调用模块。
        allowed_modules: &["src-tauri/src/ai/llm.rs", "src-tauri/src/ai/model_call.rs"],
    },
];

/// 上传用户内容体的原语：出现这些即意味着把文件内容放进请求体。
pub const FILE_BODY_UPLOAD_PRIMITIVES: [&str; 3] = ["multipart", "Part::bytes", "Part::file"];

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn repository_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn there_are_exactly_two_user_content_upload_exceptions() {
        assert_eq!(
            USER_CONTENT_UPLOAD_EXCEPTIONS.len(),
            2,
            "例外必须恰好两条：音频直传与最小文本"
        );
        let keys: Vec<&str> = USER_CONTENT_UPLOAD_EXCEPTIONS
            .iter()
            .map(|exception| exception.key)
            .collect();
        assert_eq!(keys, vec!["cloud_asr_audio", "llm_minimal_text"]);
        for exception in USER_CONTENT_UPLOAD_EXCEPTIONS {
            assert!(
                exception.requires_user_authorization,
                "例外 {} 必须要求用户显式授权",
                exception.key
            );
            assert!(
                !exception.description.trim().is_empty(),
                "例外 {} 必须有可评审的说明",
                exception.key
            );
            assert!(
                !exception.allowed_modules.is_empty(),
                "例外 {} 必须声明允许的模块",
                exception.key
            );
            for module in exception.allowed_modules {
                assert!(
                    repository_root().join(module).is_file(),
                    "例外 {} 声明的模块不存在：{module}",
                    exception.key
                );
            }
        }
    }

    #[test]
    fn file_body_uploads_only_happen_inside_a_registered_exception() {
        let root = repository_root();
        let allowed: Vec<&str> = USER_CONTENT_UPLOAD_EXCEPTIONS
            .iter()
            .flat_map(|exception| exception.allowed_modules.iter().copied())
            .collect();
        let mut checked = 0_usize;
        let mut violations = Vec::new();
        // 登记处自身声明了这些模式名，它不发起上传，因此排除扫描。
        const REGISTRY_MODULE: &str = "src/privacy.rs";
        for directory in ["src", "src-tauri/src"] {
            collect_rust_files(&root.join(directory), &mut |path, contents| {
                let relative_path = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if relative_path == REGISTRY_MODULE {
                    return;
                }
                for primitive in FILE_BODY_UPLOAD_PRIMITIVES {
                    if !contents.contains(primitive) {
                        continue;
                    }
                    let relative = path
                        .strip_prefix(&root)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .replace('\\', "/");
                    checked += 1;
                    if !allowed.iter().any(|module| relative == *module) {
                        violations.push(format!("{relative} 含上传原语 {primitive}"));
                    }
                }
            });
        }
        assert!(
            violations.is_empty(),
            "上传用户内容的原语只允许出现在登记过的模块：{violations:?}"
        );
        // 云 ASR 的音频直传必须仍然存在，否则这条守卫会因"什么都没扫到"而失去意义。
        assert!(
            checked >= 1,
            "未扫描到任何上传原语，守卫本身失效（checked={checked}）"
        );
    }

    #[test]
    fn base_capabilities_never_touch_a_cloud_client() {
        // 录制、预览、封面与本地 ASR 不得引用任何 API 客户端：这些能力一旦拿到客户端，
        // 就具备了"顺手上传"的可能，边界也就不再是结构性的。
        let root = repository_root();
        let base_capability_modules = [
            "src/recorder.rs",
            "src/manager.rs",
            "src/resolver.rs",
            "src/profile_resolver.rs",
            "src-tauri/src/preview.rs",
            "src-tauri/src/thumbnail.rs",
            "src-tauri/src/supervisor.rs",
            "src-tauri/src/ai/processor.rs",
        ];
        for module in base_capability_modules {
            let path = root.join(module);
            assert!(path.is_file(), "基础能力模块必须存在：{module}");
            let contents = std::fs::read_to_string(&path).expect("读取模块");
            // 只禁"上传能力"与已认证云客户端：`reqwest` 本身不禁——
            // 直播间解析等基础能力需要普通 GET，而它不携带任何用户内容。
            for forbidden in ["ApiClient", "multipart", "Part::bytes", "Part::file"] {
                assert!(
                    !contents.contains(forbidden),
                    "{module} 不得引用「{forbidden}」，否则基础能力可能上传用户内容"
                );
            }
        }
        // 云端转写是唯一允许持有上传客户端的模块（且已登记为显式例外）。
        let cloud = std::fs::read_to_string(root.join("src/asr/cloud.rs")).expect("读取云 ASR");
        assert!(cloud.contains("multipart"), "云转写必须仍是唯一上传点");
    }

    fn collect_rust_files(directory: &Path, visit: &mut dyn FnMut(&Path, &str)) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, visit);
                continue;
            }
            if path.extension().and_then(|value| value.to_str()) != Some("rs") {
                continue;
            }
            if let Ok(contents) = std::fs::read_to_string(&path) {
                visit(&path, &contents);
            }
        }
    }
}
