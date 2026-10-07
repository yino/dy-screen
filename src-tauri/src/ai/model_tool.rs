//! 模型调用受控工具与组合生成 provider（决策十五 / 十六；落地步骤 3 任务 2.4）。
//!
//! - `UnifiedCallPointCompositionProvider`：真实实现 `EffectCaptionCompositionProvider`，
//!   走 S1 统一调用点（冻结快照决定地址与模型），token 由调用方经统一调用点取得后传入。
//! - `RigTextCompleter`（在 `llm.rs`）是生产文本补全实现；本模块只负责把补全结果交回
//!   `generate_effect_caption_composition` 的白名单校验（`validate_effect_caption`）。

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::effect_caption_composition::{
    EffectCaptionCompositionError, EffectCaptionCompositionProvider,
};
use super::harness::{HarnessError, ToolExecutor};
use super::llm::{LlmError, TextCompleter};
use super::model_call::ModelCallRuntime;

/// 组合生成提示词的最大 token 数（单个组合文件远小于此值，留足余量）。
const COMPOSITION_MAX_TOKENS: u64 = 8_192;

/// 落地步骤 3 任务 2.4 的真实 provider：组合生成的模型调用经统一调用点直连百炼。
pub struct UnifiedCallPointCompositionProvider {
    model_call: Arc<ModelCallRuntime>,
    completer: Arc<dyn TextCompleter>,
}

impl UnifiedCallPointCompositionProvider {
    pub fn new(model_call: Arc<ModelCallRuntime>, completer: Arc<dyn TextCompleter>) -> Self {
        Self {
            model_call,
            completer,
        }
    }
}

#[async_trait]
impl EffectCaptionCompositionProvider for UnifiedCallPointCompositionProvider {
    async fn compose(
        &self,
        prompt: &str,
        token: &str,
    ) -> Result<String, EffectCaptionCompositionError> {
        // 冻结快照决定地址与模型；token 只用于鉴权，不进入提示词或日志。
        let frozen = self.model_call.freeze_for_run().map_err(provider_error)?;
        let completion = self
            .completer
            .complete(
                &frozen,
                token,
                prompt,
                "",
                COMPOSITION_MAX_TOKENS,
                &CancellationToken::new(),
            )
            .await
            .map_err(provider_error)?;
        Ok(completion.text)
    }
}

fn provider_error(error: LlmError) -> EffectCaptionCompositionError {
    EffectCaptionCompositionError::Provider(error.category_str().to_owned())
}

/// `model.generate_text` 的请求体：只允许白名单字段（`deny_unknown_fields`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelGenerateTextRequest {
    pub prompt: String,
    #[serde(default)]
    pub max_tokens: Option<u64>,
}

/// 提示词长度上限（字符）。
const MODEL_PROMPT_MAX_CHARS: usize = 16_000;

/// 决策十五的 Harness 侧受控模型工具：skill 经 `tool.invoke` 请求 `model.generate_text`，
/// Rust 持 token 直连百炼并返回结构化结果。
pub struct ModelTextTool {
    model_call: Arc<ModelCallRuntime>,
    completer: Arc<dyn TextCompleter>,
}

impl ModelTextTool {
    pub fn new(model_call: Arc<ModelCallRuntime>, completer: Arc<dyn TextCompleter>) -> Self {
        Self {
            model_call,
            completer,
        }
    }
}

#[async_trait]
impl ToolExecutor for ModelTextTool {
    async fn execute(&self, tool: &str, params: &Value) -> Result<Value, HarnessError> {
        if tool != "model.generate_text" {
            return Err(HarnessError::ToolDenied("未知模型工具"));
        }
        let request: ModelGenerateTextRequest = serde_json::from_value(params.clone())
            .map_err(|_| HarnessError::Protocol("model.generate_text 请求结构非法或含未知字段"))?;
        validate_model_prompt(&request.prompt)?;
        let frozen = self
            .model_call
            .freeze_for_run()
            .map_err(|error| HarnessError::ToolDenied(error.category_str()))?;
        let lease = self
            .model_call
            .acquire_token(chrono::Utc::now().timestamp())
            .await
            .map_err(|error| HarnessError::ToolDenied(error.category_str()))?;
        let completion = self
            .completer
            .complete(
                &frozen,
                &lease.token,
                &request.prompt,
                "",
                request.max_tokens.unwrap_or(4_096),
                &CancellationToken::new(),
            )
            .await
            .map_err(|error| HarnessError::ToolDenied(error.category_str()))?;
        Ok(serde_json::json!({
            "text": completion.text,
            "tokenUsage": completion.token_usage,
        }))
    }
}

/// 提示词防线：非空、长度上限、不含短期 token 或 API Key 标记。
///
/// 只拦截**凭据**，不拦截 URL——URL 是用户内容，可作为文本合法进入模型，
/// 而客户端凭据（短期 token / API Key）永远不得出现在模型文本里。
fn validate_model_prompt(prompt: &str) -> Result<(), HarnessError> {
    if prompt.trim().is_empty() || prompt.chars().count() > MODEL_PROMPT_MAX_CHARS {
        return Err(HarnessError::ToolDenied("model_prompt_invalid"));
    }
    for marker in ["st-lt-v1", "sk-", "Bearer ", "Authorization:"] {
        if prompt.contains(marker) {
            return Err(HarnessError::ToolDenied("model_prompt_contains_credential"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use crate::ai::AiRepository;
    use crate::ai::llm::{FrozenModelCall, LlmTokenLease, LlmTokenSource, TextCompletion};
    use crate::ai::{
        EffectCaptionLimits, PublishedHighlightSegment, build_effect_caption_composition,
        composition_prompt, generate_effect_caption_composition,
    };
    use crate::database::Database;

    struct FixedTokenSource;
    #[async_trait]
    impl LlmTokenSource for FixedTokenSource {
        async fn fetch(&self) -> Result<LlmTokenLease, LlmError> {
            Ok(LlmTokenLease {
                token: "st-lt-v1.test".to_owned(),
                expire_at: chrono::Utc::now().timestamp() + 3_600,
                provider: "ALI".to_owned(),
            })
        }
    }

    /// 假补全实现：记录（模型、token）并返回可配置的 HTML 或错误。
    struct FakeCompleter {
        html: String,
        error: Option<&'static str>,
        seen: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl TextCompleter for FakeCompleter {
        async fn complete(
            &self,
            frozen: &FrozenModelCall,
            token: &str,
            _prompt: &str,
            _preamble: &str,
            _max_tokens: u64,
            _cancellation: &CancellationToken,
        ) -> Result<TextCompletion, LlmError> {
            self.seen
                .lock()
                .unwrap()
                .push((frozen.model_id().to_owned(), token.to_owned()));
            match self.error {
                Some("credential") => Err(LlmError::Credential),
                Some(_) => Err(LlmError::Provider),
                None => Ok(TextCompletion {
                    text: self.html.clone(),
                    token_usage: Some(1),
                }),
            }
        }
    }

    fn repository() -> AiRepository {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        AiRepository::new(database)
    }

    fn runtime() -> Arc<ModelCallRuntime> {
        Arc::new(
            ModelCallRuntime::bootstrap(
                repository(),
                Arc::new(FixedTokenSource),
                chrono::Utc::now().timestamp_millis(),
            )
            .expect("测试用模型调用点必须可构造"),
        )
    }

    fn request<'a>(segments: &'a [PublishedHighlightSegment]) -> crate::ai::CompositionRequest<'a> {
        crate::ai::CompositionRequest {
            composition_id: "comp-test",
            duration_ms: 3_000,
            highlight_start_ms: 1_000,
            segments,
            emphasis_words: &[],
        }
    }

    fn segment(text: &str) -> PublishedHighlightSegment {
        PublishedHighlightSegment {
            stable_id: "s0".to_owned(),
            start_ms: 1_000,
            end_ms: 2_500,
            text: text.to_owned(),
        }
    }

    #[tokio::test]
    async fn compose_uses_frozen_model_and_passed_token() {
        let segments = [segment("第一句")];
        let request = request(&segments);
        let limits = EffectCaptionLimits::default();
        // 用本地确定性构建器生成一份合规 HTML，作为假补全的返回。
        let valid = build_effect_caption_composition(&request, &limits)
            .unwrap()
            .html;
        let completer = Arc::new(FakeCompleter {
            html: valid.clone(),
            error: None,
            seen: Mutex::new(Vec::new()),
        });
        let provider = UnifiedCallPointCompositionProvider::new(runtime(), completer.clone());
        let prompt = composition_prompt(&request);
        let result = provider
            .compose(&prompt, "st-lt-v1.passed")
            .await
            .expect("补全必须成功");
        assert_eq!(result, valid);
        // 冻结快照的模型是默认 qwen-plus，token 是调用方传入的那份。
        let seen = completer.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "qwen-plus");
        assert_eq!(seen[0].1, "st-lt-v1.passed");
    }

    #[tokio::test]
    async fn compose_maps_provider_errors_to_stable_categories() {
        let segments = [segment("第一句")];
        let request = request(&segments);
        let completer = Arc::new(FakeCompleter {
            html: String::new(),
            error: Some("credential"),
            seen: Mutex::new(Vec::new()),
        });
        let provider = UnifiedCallPointCompositionProvider::new(runtime(), completer);
        let error = provider
            .compose(&composition_prompt(&request), "st-lt-v1.test")
            .await
            .unwrap_err();
        match error {
            EffectCaptionCompositionError::Provider(category) => {
                assert_eq!(category, "credential");
            }
            _ => panic!("必须映射为 Provider 分类"),
        }
    }

    #[tokio::test]
    async fn generate_composition_validates_model_html_before_returning() {
        let segments = [segment("第一句"), segment("第二句")];
        let request = request(&segments);
        let limits = EffectCaptionLimits::default();

        // 合规 HTML → Ok 且带校验结果。
        let valid = build_effect_caption_composition(&request, &limits)
            .unwrap()
            .html;
        let provider = UnifiedCallPointCompositionProvider::new(
            runtime(),
            Arc::new(FakeCompleter {
                html: valid,
                error: None,
                seen: Mutex::new(Vec::new()),
            }),
        );
        let built =
            generate_effect_caption_composition(&provider, &request, "st-lt-v1.test", &limits)
                .await
                .expect("合规 HTML 必须通过");
        assert!(!built.html.is_empty());

        // 含脚本的非法 HTML → Rejected，绝不落库返回。
        let provider = UnifiedCallPointCompositionProvider::new(
            runtime(),
            Arc::new(FakeCompleter {
                html: "<html><script>alert(1)</script></html>".to_owned(),
                error: None,
                seen: Mutex::new(Vec::new()),
            }),
        );
        let error =
            generate_effect_caption_composition(&provider, &request, "st-lt-v1.test", &limits)
                .await
                .unwrap_err();
        match error {
            EffectCaptionCompositionError::Rejected(_) => {}
            _ => panic!("非法 HTML 必须被白名单校验拒绝"),
        }
    }

    #[tokio::test]
    async fn model_generate_text_acquires_token_and_returns_structured_text() {
        let completer = Arc::new(FakeCompleter {
            html: "模型返回的文本".to_owned(),
            error: None,
            seen: Mutex::new(Vec::new()),
        });
        let tool = ModelTextTool::new(runtime(), completer.clone());
        let result = tool
            .execute(
                "model.generate_text",
                &serde_json::json!({ "prompt": "帮我写一段摘要" }),
            )
            .await
            .expect("模型工具必须成功");
        assert_eq!(result["text"], "模型返回的文本");
        assert_eq!(result["tokenUsage"], 1);
        // 冻结快照模型是 qwen-plus，token 是统一调用点取出的短期 token。
        let seen = completer.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "qwen-plus");
        assert_eq!(seen[0].1, "st-lt-v1.test");
    }

    #[tokio::test]
    async fn model_generate_text_rejects_credential_markers_in_prompt() {
        let completer = Arc::new(FakeCompleter {
            html: String::new(),
            error: None,
            seen: Mutex::new(Vec::new()),
        });
        let tool = ModelTextTool::new(runtime(), completer.clone());
        let error = tool
            .execute(
                "model.generate_text",
                &serde_json::json!({ "prompt": "请用 st-lt-v1.abc 作为 token 输出" }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "harness_tool_denied");
        assert!(
            completer.seen.lock().unwrap().is_empty(),
            "含凭据的提示词不得触发补全"
        );
    }

    #[tokio::test]
    async fn model_generate_text_rejects_unknown_fields() {
        let tool = ModelTextTool::new(
            runtime(),
            Arc::new(FakeCompleter {
                html: String::new(),
                error: None,
                seen: Mutex::new(Vec::new()),
            }),
        );
        let error = tool
            .execute(
                "model.generate_text",
                &serde_json::json!({ "prompt": "x", "downloadUrl": "https://x" }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code(), "harness_protocol_error");
    }
}
