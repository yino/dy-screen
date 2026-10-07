//! 统一模型调用点：短期 token 取用与模型配置的运行期持有者。
//!
//! 口径见 `add-model-gateway-and-harness-sidecar/design.md`「决策四 / 决策十」：
//!
//! - 短期 token **与云 ASR 共享同一份缓存**（`AiApi::ai_llm_ak` 内部的 `AiLlmAkCache`），
//!   本模块不新建第二套缓存，只做"取一个仍有余量的租约"这件事；
//! - 模型配置在**心跳响应到达时**重新解析，运行开始前冻结，运行期间不漂移；
//! - 解析器与 token 来源都不读环境变量、不使用可变全局：环境变量只在
//!   `ModelResolutionInputs::from_non_production_environment`（非生产 feature）读一次。

use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::activation::ActivationService;
use crate::api::ApiError;

use super::llm::{
    FrozenModelCall, LlmError, LlmTokenLease, LlmTokenSource, ModelCallResolver, ModelCallSnapshot,
    ModelResolutionInputs, acquire_llm_token,
};
use super::repository::AiRepository;

/// 生产环境的短期 token 来源：走既有激活凭据与 `GET client/ai/llm-ak`。
///
/// 它刻意不缓存、不落盘：缓存由 `AiApi` 内部负责，token 只在内存中流转。
#[derive(Clone)]
pub struct ActivationLlmTokenSource {
    activation: ActivationService,
}

impl ActivationLlmTokenSource {
    pub fn new(activation: ActivationService) -> Self {
        Self { activation }
    }
}

/// 把服务端错误映射为可区分的模型调用错误分类：
/// 授权/激活类 → `Credential`（需重新激活）；传输/超时 → `Temporary`（可重试）；其余 → `Provider`。
fn map_token_api_error(error: &ApiError) -> LlmError {
    match error {
        ApiError::Transport | ApiError::Timeout => LlmError::Temporary,
        ApiError::Configuration | ApiError::InvalidResponse => LlmError::Provider,
        ApiError::Business { .. } | ApiError::Context { .. } => {
            // 复用激活模块的撤销判定，避免两处对错误码的理解分叉。
            if crate::activation::api_error_is_revocation(error) {
                LlmError::Credential
            } else {
                match error.code() {
                    // 未开通/未授权类业务码同样按凭据处理，让界面显示可区分的授权原因。
                    Some(1002..=1006 | 2002..=2004) => LlmError::Credential,
                    _ => LlmError::Provider,
                }
            }
        }
    }
}

impl ActivationLlmTokenSource {
    /// 只读缓存状态（不发网络请求）。
    pub fn cached_lease(&self, now_unix_seconds: i64) -> Option<LlmTokenLease> {
        let (device_id, activate_code) = self.activation.credentials()?;
        let response = self
            .activation
            .api()
            .cached_ai_llm_ak(&device_id, &activate_code)?;
        let lease = LlmTokenLease {
            token: response.ak,
            expire_at: response.expire_at,
            provider: response.provider,
        };
        lease.usable_at(now_unix_seconds).then_some(lease)
    }
}

#[async_trait]
impl LlmTokenSource for ActivationLlmTokenSource {
    fn cached(&self, now_unix_seconds: i64) -> Option<LlmTokenLease> {
        self.cached_lease(now_unix_seconds)
    }

    async fn fetch(&self) -> Result<LlmTokenLease, LlmError> {
        let (device_id, activate_code) =
            self.activation.credentials().ok_or(LlmError::Credential)?;
        let response = self
            .activation
            .api()
            .ai_llm_ak(&device_id, &activate_code)
            .await
            .map_err(|error| map_token_api_error(&error))?;
        Ok(LlmTokenLease {
            token: response.ak,
            expire_at: response.expire_at,
            provider: response.provider,
        })
    }
}

/// 短期 token 的**只读**状态（不含 token 本体）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenStatus {
    pub cached: bool,
    pub provider: Option<String>,
    pub expires_in_seconds: Option<i64>,
}

/// 运行期持有的模型调用点。
///
/// 所有模块都经它取"冻结快照 + 短期 token"，因此"改一处即改全部模型"成立：
/// 三条链路不再各自读设置、各自决定地址或模型。
pub struct ModelCallRuntime {
    repository: AiRepository,
    tokens: Arc<dyn LlmTokenSource>,
    resolver: RwLock<ModelCallResolver>,
}

impl ModelCallRuntime {
    /// 启动时解析一次（冷启动立即可用，不等第一次心跳）。
    pub fn bootstrap(
        repository: AiRepository,
        tokens: Arc<dyn LlmTokenSource>,
        now_ms: i64,
    ) -> Result<Self, LlmError> {
        let settings = repository.get_llm_provider_settings(true)?;
        let inputs = ModelResolutionInputs::from_non_production_environment();
        let resolver = ModelCallResolver::bootstrap(&settings, &inputs, now_ms)?;
        let runtime = Self {
            repository,
            tokens,
            resolver: RwLock::new(resolver),
        };
        // 冷启动也要把来源与时间落盘：设置页在重启后立刻有值可显示。
        {
            let guard = runtime.resolver.read().map_err(|_| LlmError::Temporary)?;
            runtime.persist_resolution(guard.current(), guard.next_resolution_at_ms())?;
        }
        Ok(runtime)
    }

    /// 把本次解析结果写回设置表，使设置页在重启后仍能显示来源与时间。
    fn persist_resolution(
        &self,
        snapshot: &ModelCallSnapshot,
        next_resolution_at_ms: i64,
    ) -> Result<(), LlmError> {
        let mut settings = self.repository.get_llm_provider_settings(true)?;
        settings.model_id = snapshot.model_id.clone();
        settings.model_source = Some(snapshot.source.as_str().to_owned());
        settings.resolved_at = Some(chrono::Utc::now().to_rfc3339());
        settings.next_resolution_at = Some(
            chrono::DateTime::from_timestamp_millis(next_resolution_at_ms)
                .map(|value| value.to_rfc3339())
                .unwrap_or_default(),
        );
        self.repository.save_llm_provider_settings(&settings)?;
        Ok(())
    }

    /// 只读的短期 token 状态：不发起任何网络请求，绝不返回 token 本体。
    pub fn token_status(&self, now_unix_seconds: i64) -> TokenStatus {
        match self.tokens.cached(now_unix_seconds) {
            Some(lease) => TokenStatus {
                cached: true,
                provider: Some(lease.provider),
                expires_in_seconds: Some(lease.expire_at - now_unix_seconds),
            },
            None => TokenStatus {
                cached: false,
                provider: None,
                expires_in_seconds: None,
            },
        }
    }

    /// 当前生效快照（设置页展示"当前模型 + 解析来源 + 下次解析时间"用）。
    pub fn snapshot(&self) -> Result<ModelCallSnapshot, LlmError> {
        Ok(self
            .resolver
            .read()
            .map_err(|_| LlmError::Temporary)?
            .current()
            .clone())
    }

    /// 下次心跳边界解析时间。
    pub fn next_resolution_at_ms(&self) -> Result<i64, LlmError> {
        Ok(self
            .resolver
            .read()
            .map_err(|_| LlmError::Temporary)?
            .next_resolution_at_ms())
    }

    /// 运行开始前冻结；进行中的运行不受后续配置切换影响。
    pub fn freeze_for_run(&self) -> Result<FrozenModelCall, LlmError> {
        Ok(self
            .resolver
            .read()
            .map_err(|_| LlmError::Temporary)?
            .freeze_for_run())
    }

    /// 按**运行记录**里的模型冻结（续跑/恢复用）。
    ///
    /// 同一运行 MUST 复用产出它时的模型：即使设置已切换，续跑也不得漂移，
    /// 否则运行指纹与实际调用不一致、历史结果无法对账。
    pub fn freeze_with_model(&self, model_id: &str) -> Result<FrozenModelCall, LlmError> {
        let mut snapshot = self
            .resolver
            .read()
            .map_err(|_| LlmError::Temporary)?
            .current()
            .clone();
        if !model_id.trim().is_empty() {
            snapshot.model_id = model_id.trim().to_owned();
        }
        Ok(FrozenModelCall::from_snapshot(snapshot))
    }

    /// 心跳响应到达时重新解析；返回模型/地址是否变化（变化即相关缓存与指纹失效）。
    pub fn on_heartbeat(&self, now_ms: i64) -> Result<bool, LlmError> {
        let settings = self.repository.get_llm_provider_settings(true)?;
        let inputs = ModelResolutionInputs::from_non_production_environment();
        let (changed, snapshot, next_resolution_at_ms) = {
            let mut resolver = self.resolver.write().map_err(|_| LlmError::Temporary)?;
            let changed = resolver.apply_heartbeat(&settings, &inputs, now_ms)?;
            (
                changed,
                resolver.current().clone(),
                resolver.next_resolution_at_ms(),
            )
        };
        self.persist_resolution(&snapshot, next_resolution_at_ms)?;
        Ok(changed)
    }

    /// 取一个仍有余量的短期 token（余量不足时重取一次）。
    pub async fn acquire_token(&self, now_ms: i64) -> Result<LlmTokenLease, LlmError> {
        acquire_llm_token(self.tokens.as_ref(), now_ms).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::activation::api_error_is_revocation;
    use crate::api::ApiError;
    use crate::database::Database;

    fn repository() -> AiRepository {
        let database = Database::open_in_memory().expect("in-memory database");
        database.migrate().expect("migrate");
        AiRepository::new(database)
    }

    enum Reply {
        Lease(LlmTokenLease),
        Temporary,
    }

    struct ScriptedSource {
        replies: Vec<Reply>,
        calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmTokenSource for ScriptedSource {
        async fn fetch(&self) -> Result<LlmTokenLease, LlmError> {
            let index = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.replies.get(index) {
                Some(Reply::Lease(lease)) => Ok(lease.clone()),
                Some(Reply::Temporary) | None => Err(LlmError::Temporary),
            }
        }
    }

    fn lease(expire_at: i64) -> LlmTokenLease {
        LlmTokenLease {
            token: "st-lt-v1.shared".to_owned(),
            expire_at,
            provider: "ALI".to_owned(),
        }
    }

    #[test]
    fn bootstrap_and_heartbeat_persist_resolution_metadata_for_the_settings_page() {
        let repository = repository();
        let runtime = ModelCallRuntime::bootstrap(
            repository.clone(),
            Arc::new(ScriptedSource {
                replies: Vec::new(),
                calls: AtomicUsize::new(0),
            }),
            1_000,
        )
        .unwrap();
        let settings = repository.get_llm_provider_settings(true).unwrap();
        assert_eq!(settings.model_source.as_deref(), Some("build_default"));
        assert!(settings.resolved_at.is_some(), "冷启动必须落解析时间");
        assert!(
            settings.next_resolution_at.is_some(),
            "冷启动必须落下一次解析时间"
        );

        // 用户改设置 → 心跳边界解析后来源变成 local_override 并落盘。
        let mut changed = settings.clone();
        changed.model_id = "qwen-max".to_owned();
        repository.save_llm_provider_settings(&changed).unwrap();
        assert!(runtime.on_heartbeat(90_000).unwrap());
        let after = repository.get_llm_provider_settings(true).unwrap();
        assert_eq!(after.model_id, "qwen-max");
        assert_eq!(after.model_source.as_deref(), Some("local_override"));
        assert_ne!(
            after.resolved_at, settings.resolved_at,
            "解析时间必须更新，供设置页显示"
        );
    }

    #[test]
    fn token_status_is_read_only_and_never_exposes_the_token_body() {
        struct CachedSource;
        #[async_trait]
        impl LlmTokenSource for CachedSource {
            async fn fetch(&self) -> Result<LlmTokenLease, LlmError> {
                Err(LlmError::Temporary)
            }
            fn cached(&self, _now: i64) -> Option<LlmTokenLease> {
                Some(lease(1_600))
            }
        }
        let runtime = ModelCallRuntime::bootstrap(repository(), Arc::new(CachedSource), 0).unwrap();
        let status = runtime.token_status(1_000);
        assert!(status.cached);
        assert_eq!(status.provider.as_deref(), Some("ALI"));
        assert_eq!(status.expires_in_seconds, Some(600));
        let json = serde_json::to_string(&status).unwrap();
        assert!(!json.contains("st-lt-v1"), "状态里不得含 token：{json}");

        // 无缓存：只报未缓存，不发起任何网络请求（fetch 会返回 Temporary，不应被触发）。
        struct EmptySource;
        #[async_trait]
        impl LlmTokenSource for EmptySource {
            async fn fetch(&self) -> Result<LlmTokenLease, LlmError> {
                Err(LlmError::Temporary)
            }
        }
        let runtime = ModelCallRuntime::bootstrap(repository(), Arc::new(EmptySource), 0).unwrap();
        let status = runtime.token_status(1_000);
        assert!(!status.cached);
        assert!(status.provider.is_none());
        assert!(status.expires_in_seconds.is_none());
    }

    #[test]
    fn runtime_bootstraps_from_persisted_settings_and_exposes_resolution_time() {
        let runtime = ModelCallRuntime::bootstrap(
            repository(),
            Arc::new(ScriptedSource {
                replies: Vec::new(),
                calls: AtomicUsize::new(0),
            }),
            1_000,
        )
        .expect("冷启动必须成功");
        let snapshot = runtime.snapshot().unwrap();
        // 全新库的默认取值即构建期默认模型。
        assert_eq!(snapshot.model_id, "qwen-plus");
        assert_eq!(runtime.next_resolution_at_ms().unwrap(), 1_000 + 90_000);
    }

    #[test]
    fn heartbeat_boundary_refresh_updates_snapshot_and_reports_change() {
        let repository = repository();
        let runtime = ModelCallRuntime::bootstrap(
            repository.clone(),
            Arc::new(ScriptedSource {
                replies: Vec::new(),
                calls: AtomicUsize::new(0),
            }),
            0,
        )
        .unwrap();
        let frozen = runtime.freeze_for_run().unwrap();
        assert_eq!(frozen.model_id(), "qwen-plus");

        // 用户改设置（模型标识）→ 下一次心跳边界才生效。
        let mut settings = repository.get_llm_provider_settings(true).unwrap();
        settings.model_id = "qwen-max".to_owned();
        repository.save_llm_provider_settings(&settings).unwrap();
        assert_eq!(
            runtime.snapshot().unwrap().model_id,
            "qwen-plus",
            "心跳边界之前不得生效"
        );
        assert!(runtime.on_heartbeat(90_000).unwrap(), "模型变化必须上报");
        assert_eq!(runtime.snapshot().unwrap().model_id, "qwen-max");
        // 已冻结的运行不受影响。
        assert_eq!(frozen.model_id(), "qwen-plus");

        // 同一模型再次心跳 → 不报告变化。
        assert!(!runtime.on_heartbeat(180_000).unwrap());
    }

    #[tokio::test]
    async fn runtime_shares_the_token_lease_and_retries_once_when_expiring() {
        let source = Arc::new(ScriptedSource {
            replies: vec![Reply::Lease(lease(1_010)), Reply::Lease(lease(1_900))],
            calls: AtomicUsize::new(0),
        });
        let runtime = ModelCallRuntime::bootstrap(repository(), source.clone(), 0).unwrap();
        let acquired = runtime.acquire_token(1_000).await.expect("重取后必须成功");
        assert_eq!(acquired.provider, "ALI");
        assert_eq!(source.calls.load(Ordering::SeqCst), 2);
        // 诊断标签不得含 token 本体。
        assert!(!acquired.diagnostic_label(1_000).contains("st-lt-v1"));
    }

    #[tokio::test]
    async fn runtime_surfaces_credential_failure_when_tokens_never_have_margin() {
        let source = Arc::new(ScriptedSource {
            replies: vec![Reply::Lease(lease(1_000)), Reply::Lease(lease(1_000))],
            calls: AtomicUsize::new(0),
        });
        let runtime = ModelCallRuntime::bootstrap(repository(), source.clone(), 0).unwrap();
        let error = runtime.acquire_token(1_000).await.unwrap_err();
        assert_eq!(error.category_str(), "credential");
        assert_eq!(source.calls.load(Ordering::SeqCst), 2, "只重取一次");
    }

    #[tokio::test]
    async fn runtime_propagates_a_token_source_failure_without_retrying() {
        let source = Arc::new(ScriptedSource {
            replies: vec![Reply::Temporary],
            calls: AtomicUsize::new(0),
        });
        let runtime = ModelCallRuntime::bootstrap(repository(), source.clone(), 0).unwrap();
        let error = runtime.acquire_token(1_000).await.unwrap_err();
        assert_eq!(error.category_str(), "temporary");
        assert_eq!(
            source.calls.load(Ordering::SeqCst),
            1,
            "来源本身报错时必须直接上抛，不做无意义重取"
        );
    }

    #[test]
    fn token_api_errors_map_to_distinguishable_categories() {
        assert_eq!(
            map_token_api_error(&ApiError::Transport).category_str(),
            "temporary"
        );
        assert_eq!(
            map_token_api_error(&ApiError::Timeout).category_str(),
            "temporary"
        );
        assert_eq!(
            map_token_api_error(&ApiError::InvalidResponse).category_str(),
            "provider"
        );
        // 未开通/撤销类业务码 → 凭据类，界面可显示"需重新激活/未开通"。
        assert_eq!(
            map_token_api_error(&ApiError::Business {
                code: 1003,
                message: "未开通".to_owned(),
            })
            .category_str(),
            "credential"
        );
        assert_eq!(
            map_token_api_error(&ApiError::Business {
                code: 500,
                message: "内部错误".to_owned(),
            })
            .category_str(),
            "provider"
        );
        // 撤销与未授权判定必须与激活模块同源。
        let revoked = ApiError::Business {
            code: 1002,
            message: "revoked".to_owned(),
        };
        assert!(api_error_is_revocation(&revoked));
        assert_eq!(map_token_api_error(&revoked).category_str(), "credential");
    }

    #[test]
    fn two_runtimes_do_not_share_mutable_state() {
        // 并发运行必须互不影响：解析结果存在各自的运行时实例里，没有可变全局。
        let first = ModelCallRuntime::bootstrap(
            repository(),
            Arc::new(ScriptedSource {
                replies: Vec::new(),
                calls: AtomicUsize::new(0),
            }),
            0,
        )
        .unwrap();
        let second_repository = repository();
        let mut settings = second_repository.get_llm_provider_settings(true).unwrap();
        settings.model_id = "qwen-turbo".to_owned();
        second_repository
            .save_llm_provider_settings(&settings)
            .unwrap();
        let second = ModelCallRuntime::bootstrap(
            second_repository,
            Arc::new(ScriptedSource {
                replies: Vec::new(),
                calls: AtomicUsize::new(0),
            }),
            0,
        )
        .unwrap();
        assert_eq!(first.snapshot().unwrap().model_id, "qwen-plus");
        assert_eq!(second.snapshot().unwrap().model_id, "qwen-turbo");
        assert_ne!(
            first.freeze_for_run().unwrap().fingerprint_input(),
            second.freeze_for_run().unwrap().fingerprint_input()
        );
    }
}
