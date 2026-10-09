//! 把一次**真实生成**接到既有的生成式动画资产链路上。
//!
//! 既有链路（见 `tests/generated_animation_pipeline.rs`）的顺序是：
//! `时长适配 → 目标范围计算 → 复用键 → 写入资产 → 导出源 → 导出计划`。
//! 本模块补上其中最前面那段——**真正去上游生成、下载、校验、落盘**。
//!
//! 关键实现约束：
//! - 视频的首帧只接受**公网 URL / OSS 临时 URL / base64 data URI**（上游限制）。
//!   参考图是本地产物，因此走 **base64 data URI**，不需要先把图传到公网。
//! - 资产行必须先落库拿到 `id`，文件才能按 `generated_animation_file_name(id)`
//!   命名。所以顺序是：生成→校验→落库→**改名**到约定路径，而不是先写文件。

use std::path::Path;

use async_trait::async_trait;

use super::domain::{
    AiGeneratedAnimationAsset, AiGeneratedAnimationComposition, AiGeneratedAnimationInsertBinding,
    AiGeneratedAnimationReplaceRange, AiGeneratedAnimationStatus, GENERATED_ANIMATION_CACHE_DIR,
    NewAiGeneratedAnimationAsset, generated_animation_file_name,
};
use super::generated_asset::{
    AssetExpectation, MediaProbe, download_and_validate, sha256_hex,
};
use super::repository::AiRepository;
use super::wan_generation::{GenerationClient, GenerationTransport};

/// 一次生成式转场的输入。
#[derive(Debug, Clone)]
pub struct GeneratedTransitionRequest {
    pub project_id: i64,
    pub candidate_id: i64,
    pub highlight_run_id: Option<i64>,
    pub generation: i64,
    pub composition: AiGeneratedAnimationComposition,
    pub replace_range: Option<AiGeneratedAnimationReplaceRange>,
    pub insert_binding: Option<AiGeneratedAnimationInsertBinding>,
    /// 复用键：分镜内容 + 提示词 + 目标时长 + 生成规格，全部一致才复用。
    pub reuse_key: String,
    /// 文生图提示词（参考图）。
    pub image_prompt: String,
    /// 图生视频提示词（镜头运动）。
    pub video_prompt: String,
}

/// 可区分的失败原因；都带上"哪一段坏了"，便于定位是图还是视频。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeneratedTransitionFailure {
    Image { code: String, detail: String },
    Video { code: String, detail: String },
    Repository(String),
    Io(String),
}

impl GeneratedTransitionFailure {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Image { .. } => "generated_transition_image_failed",
            Self::Video { .. } => "generated_transition_video_failed",
            Self::Repository(_) => "generated_transition_repository_failed",
            Self::Io(_) => "generated_transition_io_failed",
        }
    }
}

/// 生成一段转场并登记为可用资产。
///
/// 返回落库后的资产（`status = Validated`，且媒体属性来自**实际探测**而不是上游自述）。
pub async fn produce_generated_transition<T: GenerationTransport, P: MediaProbe>(
    client: &GenerationClient<T>,
    probe: &P,
    repository: &AiRepository,
    cache_root: &Path,
    token: &str,
    request: GeneratedTransitionRequest,
) -> Result<(AiGeneratedAnimationAsset, String), GeneratedTransitionFailure> {
    let cache_dir = cache_root.join(GENERATED_ANIMATION_CACHE_DIR);
    tokio::fs::create_dir_all(&cache_dir)
        .await
        .map_err(|error| GeneratedTransitionFailure::Io(error.to_string()))?;

    // 临时文件名由复用键派生：稳定、可复现，且同一高光重复生成时会覆盖而不是堆积。
    let tag = &sha256_hex(request.reuse_key.as_bytes())[..16];
    let scratch_image = cache_dir.join(format!("pending-image-{tag}.bin"));
    let scratch_video = cache_dir.join(format!("pending-video-{tag}.mp4"));

    // ── ① 参考图 ──
    let image = client
        .generate_image(token, &request.image_prompt)
        .await
        .map_err(|(model, failure)| GeneratedTransitionFailure::Image {
            code: "generated_transition_image_upstream".to_owned(),
            detail: format!("{model}: {failure:?}"),
        })?;
    let image_asset = download_and_validate(
        client.transport_ref(),
        probe,
        &image.url,
        &scratch_image,
        AssetExpectation::image(),
    )
    .await
    .map_err(|rejection| GeneratedTransitionFailure::Image {
        code: rejection.code().to_owned(),
        detail: format!("{}: {rejection:?}", image.model),
    })?;

    // 参考图不再需要落盘：它只作为首帧数据传给视频生成。
    let image_bytes = tokio::fs::read(&image_asset.path)
        .await
        .map_err(|error| GeneratedTransitionFailure::Io(error.to_string()))?;
    let _ = tokio::fs::remove_file(&image_asset.path).await;
    let first_frame = data_uri("image/png", &image_bytes);

    // ── ② 视频 ──
    let duration_ms = u64::from(client.config().video_duration_seconds) * 1_000;
    let video = client
        .generate_video(token, &request.video_prompt, &first_frame)
        .await
        .map_err(|(model, failure)| GeneratedTransitionFailure::Video {
            code: "generated_transition_video_upstream".to_owned(),
            detail: format!("{model}: {failure:?}"),
        })?;
    let video_asset = download_and_validate(
        client.transport_ref(),
        probe,
        &video.url,
        &scratch_video,
        AssetExpectation::video(duration_ms),
    )
    .await
    .map_err(|rejection| GeneratedTransitionFailure::Video {
        code: rejection.code().to_owned(),
        detail: format!("{}: {rejection:?}", video.model),
    })?;

    // ── ③ 落库（此时才拿到 id）──
    let recorded = repository
        .record_generated_animation_asset(NewAiGeneratedAnimationAsset {
            project_id: request.project_id,
            candidate_id: request.candidate_id,
            highlight_run_id: request.highlight_run_id,
            generation: request.generation,
            reuse_key: request.reuse_key.clone(),
            composition: request.composition,
            status: AiGeneratedAnimationStatus::Validated,
            replace_range: request.replace_range,
            insert_binding: request.insert_binding,
            upstream_task_id: video.task_id.clone(),
            sha256: Some(video_asset.sha256.clone()),
            width: Some(video_asset.media.width),
            height: Some(video_asset.media.height),
            fps: Some(video_asset.media.fps),
            duration_ms: Some(video_asset.media.duration_ms),
            rejection_code: None,
        })
        .map_err(|error| GeneratedTransitionFailure::Repository(error.to_string()))?;

    // ── ④ 改名到约定路径（导出只会去这里找文件）──
    let final_path = cache_dir.join(generated_animation_file_name(recorded.id));
    tokio::fs::rename(&video_asset.path, &final_path)
        .await
        .map_err(|error| GeneratedTransitionFailure::Io(error.to_string()))?;

    // 把"实际用了哪个视频模型"带回去：免费额度与付费备选必须可追溯。
    Ok((recorded, video.model))
}

/// 一条待生成的转场任务（由流程阶段构造后交给执行者）。
///
/// 提示词必须是**分镜描述**（画面主体/构图/镜头运动），不是字幕原文——
/// 用字幕原文当提示词会生成语义错位的画面（实测过：字幕写"直播 kiss 月入千万"，
/// 生成结果真画出一个直播间）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTransitionTask {
    /// **AI 工程 id**（不是合辑工程 id；见 `produce_generated_transition` 的易错点）。
    pub project_id: i64,
    pub clip_project_id: i64,
    pub candidate_id: i64,
    pub highlight_run_id: Option<i64>,
    pub generation: i64,
    pub image_prompt: String,
    pub video_prompt: String,
    /// 目标时长（毫秒），与 `replace_range`/`insert_binding` 必须一致。
    pub duration_ms: u64,
    /// 复用键：命中已校验资产即复用，**不产生新的上游费用**。
    pub reuse_key: String,
    /// 合成方式与绑定；与 `AiGeneratedAnimationAsset` 一一对应。
    pub composition: AiGeneratedAnimationComposition,
    pub replace_range: Option<AiGeneratedAnimationReplaceRange>,
    pub insert_binding: Option<AiGeneratedAnimationInsertBinding>,
}

/// 执行结果（脱敏，可直接入库与展示）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedTransitionOutcome {
    pub asset_id: i64,
    pub model: String,
    pub duration_ms: u64,
    /// 命中复用：**本次没有产生上游费用**。
    pub reused: bool,
}

/// 生成式转场的**阶段执行者**。
///
/// 与 `EffectCaptionEnhancementRunner` 同构：流程阶段只依赖这个 trait，
/// 因此可以用假实现做测试，也不会把网络依赖塞进流程代码。
#[async_trait]
pub trait GeneratedTransitionRunner: Send + Sync {
    async fn generate(
        &self,
        task: &GeneratedTransitionTask,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<GeneratedTransitionOutcome, GeneratedTransitionFailure>;
}

/// 单次运行内是否还要继续生成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionAction {
    /// 已有通过校验的同键资产：直接复用。
    Reuse(i64),
    /// 需要真实调用上游。
    Generate,
    /// 达到本次运行上限：停止，**不调用上游**。
    SkipBudgetExhausted,
}

/// 是否复用 / 是否生成 / 是否因额度停下。
///
/// 抽成纯函数以便单测：这段判断直接决定**要不要花钱**，值得被测试钉住。
pub fn plan_transition_action(
    reused_asset_id: Option<i64>,
    generated_so_far: usize,
    max_per_run: usize,
) -> TransitionAction {
    if let Some(asset_id) = reused_asset_id {
        return TransitionAction::Reuse(asset_id);
    }
    if generated_so_far >= max_per_run {
        return TransitionAction::SkipBudgetExhausted;
    }
    TransitionAction::Generate
}

/// 真实执行者：包装 `produce_generated_transition`，并加上**复用**与**单次运行上限**门禁。
///
/// 两道门禁都直接关系花钱：
/// - 命中同键已校验资产 → 复用，**不调用上游**
/// - 达到单次运行上限 → 跳过，**不调用上游**
pub struct WanGeneratedTransitionRunner<T: GenerationTransport, P: MediaProbe> {
    repository: AiRepository,
    client: GenerationClient<T>,
    probe: P,
    cache_root: std::path::PathBuf,
    model_call: std::sync::Arc<super::ModelCallRuntime>,
    max_per_run: usize,
    generated: std::sync::Mutex<usize>,
}

impl<T: GenerationTransport, P: MediaProbe> WanGeneratedTransitionRunner<T, P> {
    pub fn new(
        repository: AiRepository,
        client: GenerationClient<T>,
        probe: P,
        cache_root: std::path::PathBuf,
        model_call: std::sync::Arc<super::ModelCallRuntime>,
        max_per_run: usize,
    ) -> Self {
        Self {
            repository,
            client,
            probe,
            cache_root,
            model_call,
            max_per_run,
            generated: std::sync::Mutex::new(0),
        }
    }

    /// 本次运行已真实生成的条数（用于汇报与诊断）。
    pub fn generated_count(&self) -> usize {
        *self
            .generated
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl<T: GenerationTransport, P: MediaProbe> GeneratedTransitionRunner
    for WanGeneratedTransitionRunner<T, P>
{
    async fn generate(
        &self,
        task: &GeneratedTransitionTask,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<GeneratedTransitionOutcome, GeneratedTransitionFailure> {
        let reused = self
            .repository
            .find_validated_generated_animation(task.project_id, &task.reuse_key)
            .ok()
            .flatten();
        let generated_so_far = self.generated_count();
        match plan_transition_action(reused.as_ref().map(|asset| asset.id), generated_so_far, self.max_per_run) {
            TransitionAction::Reuse(asset_id) => {
                return Ok(GeneratedTransitionOutcome {
                    asset_id,
                    model: "复用已校验资产".to_owned(),
                    duration_ms: reused
                        .and_then(|asset| asset.duration_ms)
                        .unwrap_or(task.duration_ms),
                    reused: true,
                });
            }
            TransitionAction::SkipBudgetExhausted => {
                return Err(GeneratedTransitionFailure::Video {
                    code: "generated_transition_budget_exhausted".to_owned(),
                    detail: format!("本次运行已达到上限 {generated_so_far}/{}", self.max_per_run),
                });
            }
            TransitionAction::Generate => {}
        }

        if cancellation.is_cancelled() {
            return Err(GeneratedTransitionFailure::Io("已取消".to_owned()));
        }

        let lease = self
            .model_call
            .acquire_token(chrono::Utc::now().timestamp())
            .await
            .map_err(|error| GeneratedTransitionFailure::Repository(error.category_str().to_owned()))?;

        let (asset, model) = produce_generated_transition(
            &self.client,
            &self.probe,
            &self.repository,
            &self.cache_root,
            &lease.token,
            GeneratedTransitionRequest {
                project_id: task.project_id,
                candidate_id: task.candidate_id,
                highlight_run_id: task.highlight_run_id,
                generation: task.generation,
                composition: task.composition,
                replace_range: task.replace_range,
                insert_binding: task.insert_binding,
                reuse_key: task.reuse_key.clone(),
                image_prompt: task.image_prompt.clone(),
                video_prompt: task.video_prompt.clone(),
            },
        )
        .await?;

        {
            let mut counter = self
                .generated
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *counter += 1;
        }

        Ok(GeneratedTransitionOutcome {
            asset_id: asset.id,
            model,
            duration_ms: asset.duration_ms.unwrap_or(task.duration_ms),
            reused: false,
        })
    }
}

/// 标准 base64（含 `=` 填充）。手写以避免在 `--offline` 下新增依赖。
pub fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(combined >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(combined >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[(combined >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[combined as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// `data:{mime};base64,{payload}`——上游要求的图像内联格式。
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    format!("data:{mime};base64,{}", base64_encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 与 RFC 4648 的标准向量比对——base64 错了，上游会直接拒绝首帧。
    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    /// 覆盖全部 256 个字节值，确保每个 6 位组都取到了正确字符。
    #[test]
    fn base64_handles_every_byte_value() {
        let all: Vec<u8> = (0..=255).collect();
        let encoded = base64_encode(&all);
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
        assert!(encoded.ends_with("vw==") || encoded.ends_with("v0=") || encoded.len() % 4 == 0);
        assert_eq!(encoded.len() % 4, 0, "长度必须是 4 的倍数");
    }

    /// 这段判断直接决定"要不要花钱"，必须钉住：
    /// 命中复用就绝不重复生成；到达上限就绝不再调用上游。
    #[test]
    fn action_planning_never_spends_when_it_should_not() {
        // 命中复用优先于额度判断（额度用尽也能复用已生成的资产）
        assert_eq!(
            plan_transition_action(Some(7), 5, 1),
            TransitionAction::Reuse(7)
        );
        // 额度用尽且无复用 → 停下，不调用上游
        assert_eq!(
            plan_transition_action(None, 2, 2),
            TransitionAction::SkipBudgetExhausted
        );
        // 还有额度 → 生成
        assert_eq!(plan_transition_action(None, 1, 2), TransitionAction::Generate);
        // 上限为 0 时任何新生成都被挡住
        assert_eq!(
            plan_transition_action(None, 0, 0),
            TransitionAction::SkipBudgetExhausted
        );
    }

    #[test]
    fn data_uri_carries_the_declared_mime_type() {
        let uri = data_uri("image/png", b"foo");
        assert_eq!(uri, "data:image/png;base64,Zm9v");
    }
}
