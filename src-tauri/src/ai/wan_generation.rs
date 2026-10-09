//! 万相/千问「生成式转场」的真实上游客户端。
//!
//! 契约来自对上游的**实测**（不是文档推测），两条路径的模式**不同**——这是本模块
//! 存在的理由，也是此前把新旧模型混在一条路径上会报出误导性 `url error` 的原因：
//!
//! | 模型族 | 路径 | 模式 |
//! |---|---|---|
//! | 新一代图像（`qwen-image-3.0` / `qwen-image-2.1-pro` / `wan2.6-t2i`） | `multimodal-generation/generation` | **仅同步**（不带 `X-DashScope-Async`） |
//! | 旧一代图像（`qwen-image-plus` / `qwen-image`） | `text2image/image-synthesis` | **仅异步**（必须 `X-DashScope-Async: enable`） |
//! | 视频（`wan3.0-video-prime` / `wan3.0-video`） | `video-generation/video-synthesis` | **仅异步** + 轮询 `GET /tasks/{id}` |
//!
//! 把模型放到错误的模式上，上游返回的是：
//! - 错误路径 → `400 InvalidParameter: url error`（**与配额无关**，极易被误读为"没额度"）
//! - 正确路径但模式错 → `403 AccessDenied: current user api does not support asynchronous calls`
//!
//! 因此图像侧不写死"哪个模型属于哪一代"，而是**先同步多模态、失败再异步 text2image**，
//! 两条都试；这正是"异步→同步降级"的可执行版本。

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

/// 北京地域的裸域名（官方 Postman 指南确认；不需要 workspace 作用域域名）。
pub const DASHSCOPE_API_BASE_URL: &str = "https://dashscope.aliyuncs.com/api/v1";

const MULTIMODAL_PATH: &str = "/services/aigc/multimodal-generation/generation";
const TEXT2IMAGE_PATH: &str = "/services/aigc/text2image/image-synthesis";
const VIDEO_PATH: &str = "/services/aigc/video-generation/video-synthesis";

/// 生成配置：可经「设置」覆盖，默认值取实测可用的模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationConfig {
    pub base_url: String,
    /// 图像降级链（按序尝试）。默认：免费额度优先，其后付费备选。
    pub image_models: Vec<String>,
    /// 视频降级链（按序尝试）。
    pub video_models: Vec<String>,
    /// 视频分辨率档位（480P / 720P / 1080P）。**必须显式传**：上游默认是 1080P。
    pub video_resolution: String,
    /// 视频时长（秒）。上游允许 [2, 30]；转场用下限即可。
    pub video_duration_seconds: u32,
    /// 是否保留上游生成的音轨。转场是无音频视觉桥接，默认关闭。
    pub video_audio: bool,
    /// 单次运行最多生成多少张图（额度门禁）。
    pub max_images_per_run: u32,
    /// 单次运行最多生成多少秒视频（额度门禁）。
    pub max_video_seconds_per_run: u64,
}

impl Default for GenerationConfig {
    fn default() -> Self {
        Self {
            base_url: DASHSCOPE_API_BASE_URL.to_owned(),
            image_models: vec![
                "qwen-image-3.0".to_owned(),
                "qwen-image-2.1-pro".to_owned(),
                "qwen-image-plus".to_owned(),
                "qwen-image".to_owned(),
            ],
            video_models: vec!["wan3.0-video-prime".to_owned(), "wan3.0-video".to_owned()],
            video_resolution: "480P".to_owned(),
            video_duration_seconds: 2,
            video_audio: false,
            max_images_per_run: 4,
            max_video_seconds_per_run: 5,
        }
    }
}

impl GenerationConfig {
    /// 由「设置」构造。
    ///
    /// 设置是用户可改的自由文本，因此这里**必须兜底**：非法分辨率、越界时长、
    /// 空链都会回落到默认值，否则一处笔误会让整个功能静默失效或按 1080P 计费。
    pub fn from_parts(
        image_models: &str,
        video_models: &str,
        resolution: &str,
        seconds: u32,
    ) -> Self {
        let defaults = Self::default();
        let images = parse_model_chain(image_models);
        let videos = parse_model_chain(video_models);
        Self {
            image_models: if images.is_empty() {
                defaults.image_models
            } else {
                images
            },
            video_models: if videos.is_empty() {
                defaults.video_models
            } else {
                videos
            },
            video_resolution: match resolution.trim().to_ascii_uppercase().as_str() {
                "480P" => "480P".to_owned(),
                "720P" => "720P".to_owned(),
                "1080P" => "1080P".to_owned(),
                _ => defaults.video_resolution,
            },
            // 上游允许 [2, 30]；越界一律回落到默认，而不是把非法值直接发上去。
            video_duration_seconds: if (2..=30).contains(&seconds) {
                seconds
            } else {
                defaults.video_duration_seconds
            },
            video_audio: false,
            ..defaults
        }
    }
}

/// 解析逗号分隔的模型链：去空白、丢弃空项、**保序去重**。
fn parse_model_chain(raw: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter(|item| seen.insert((*item).to_owned()))
        .map(str::to_owned)
        .collect()
}

/// 上游失败的**可区分**分类。区分它们才能决定"换模型"还是"换模式"还是"停下来"。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationFailure {
    /// 免费额度耗尽 / 权限不足：该模型不可用，应换下一个模型。
    QuotaExhausted,
    /// 模式不匹配（异步头用在了仅同步的路径上，或反之）：应换模式重试。
    ModeMismatch,
    /// 路径或请求形状不对：应换路径重试。
    WrongPath,
    /// 模型在该地域/账号不存在：应换下一个模型。
    ModelUnavailable,
    /// 凭据被拒：换模型没有意义，直接失败。
    Credential,
    /// 可重试的瞬时故障。
    Transient,
    /// 其它（含响应结构不符预期）。
    Invalid,
}

impl GenerationFailure {
    /// 从 HTTP 状态与响应正文分类。判据全部来自实测样本。
    pub fn classify(status: u16, body: &str) -> Self {
        if status == 401 {
            return Self::Credential;
        }
        if body.contains("InvalidApiKey") {
            return Self::Credential;
        }
        if body.contains("does not support asynchronous calls")
            || body.contains("does not support synchronous calls")
        {
            return Self::ModeMismatch;
        }
        if body.contains("FreeTierOnly") || body.contains("AllocationQuota") {
            return Self::QuotaExhausted;
        }
        if body.contains("url error") {
            return Self::WrongPath;
        }
        if body.contains("not exist") || body.contains("not found") {
            return Self::ModelUnavailable;
        }
        if status == 429 || status >= 500 {
            return Self::Transient;
        }
        if status != 200 {
            return Self::Invalid;
        }
        Self::Invalid
    }

    /// 换模型是否有意义。
    pub fn should_try_next_model(self) -> bool {
        matches!(
            self,
            Self::QuotaExhausted | Self::ModelUnavailable | Self::WrongPath | Self::ModeMismatch
        )
    }
}

/// HTTP 传输抽象：真实实现走 reqwest，测试注入假传输，**测试不触网**。
#[async_trait]
pub trait GenerationTransport: Send + Sync {
    /// `asynchronous = true` 时带上 `X-DashScope-Async: enable`。
    async fn post(
        &self,
        url: &str,
        token: &str,
        asynchronous: bool,
        body: &Value,
    ) -> Result<(u16, String), String>;
    async fn get(&self, url: &str, token: &str) -> Result<(u16, String), String>;
}

/// reqwest 实现。
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl GenerationTransport for ReqwestTransport {
    async fn post(
        &self,
        url: &str,
        token: &str,
        asynchronous: bool,
        body: &Value,
    ) -> Result<(u16, String), String> {
        let mut request = self
            .client
            .post(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("Content-Type", "application/json");
        if asynchronous {
            request = request.header("X-DashScope-Async", "enable");
        }
        let response = request.json(body).send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        Ok((status, text))
    }

    async fn get(&self, url: &str, token: &str) -> Result<(u16, String), String> {
        let response = self
            .client
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let text = response.text().await.unwrap_or_default();
        Ok((status, text))
    }
}

/// 一次成功生成的产物描述（**只给 URL 与用量**，下载与校验由调用方负责）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedAsset {
    pub url: String,
    pub model: String,
    /// 上游报告的用量字段（图片为 `image_count`，视频为 `video_count` + 时长）。
    pub usage: String,
}

/// 生成客户端。
pub struct GenerationClient<T: GenerationTransport> {
    transport: T,
    config: GenerationConfig,
}

impl<T: GenerationTransport> GenerationClient<T> {
    pub fn new(transport: T, config: GenerationConfig) -> Self {
        Self { transport, config }
    }

    pub fn config(&self) -> &GenerationConfig {
        &self.config
    }

    /// 生成一张参考图。
    ///
    /// 图像侧**不预判模型属于哪一代**：先按同步多模态路径试，遇到
    /// `WrongPath` / `ModeMismatch` 再换异步 text2image 路径；两者都失败才换下一个模型。
    pub async fn generate_image(
        &self,
        token: &str,
        prompt: &str,
    ) -> Result<GeneratedAsset, (String, GenerationFailure)> {
        self.generate_image_with_poll(token, prompt, Duration::from_secs(15), 40)
            .await
    }

    /// 同上，但轮询间隔与次数可注入（测试用极短间隔，避免测试变慢）。
    pub async fn generate_image_with_poll(
        &self,
        token: &str,
        prompt: &str,
        poll_interval: Duration,
        poll_attempts: u32,
    ) -> Result<GeneratedAsset, (String, GenerationFailure)> {
        let mut last: Option<(String, GenerationFailure)> = None;
        for model in &self.config.image_models {
            // ① 同步多模态（新一代模型的唯一可用模式）
            let multimodal = multimodal_body(model, prompt);
            match self
                .transport
                .post(
                    &format!("{}{MULTIMODAL_PATH}", self.config.base_url),
                    token,
                    false,
                    &multimodal,
                )
                .await
            {
                Ok((200, text)) => {
                    if let Some((url, usage)) = parse_multimodal_image(&text) {
                        return Ok(GeneratedAsset {
                            url,
                            model: model.clone(),
                            usage,
                        });
                    }
                    // 解析不出图片：不在此处定论，让异步路径再试一次。
                }
                Ok((status, text)) => {
                    let failure = GenerationFailure::classify(status, &text);
                    last = Some((model.clone(), failure));
                    if !matches!(
                        failure,
                        GenerationFailure::WrongPath | GenerationFailure::ModeMismatch
                    ) {
                        // 配额/不存在：换模型才有意义。
                        if failure.should_try_next_model() {
                            continue;
                        }
                        return Err((model.clone(), failure));
                    }
                }
                // 传输失败：不在此处定论，让异步路径再试一次。
                Err(_) => {}
            }

            // ② 异步 text2image（旧一代模型的唯一可用模式）
            let text2image = text2image_body(model, prompt);
            match self
                .transport
                .post(
                    &format!("{}{TEXT2IMAGE_PATH}", self.config.base_url),
                    token,
                    true,
                    &text2image,
                )
                .await
            {
                Ok((200, text)) => {
                    let Some(task_id) = parse_task_id(&text) else {
                        last = Some((model.clone(), GenerationFailure::Invalid));
                        continue;
                    };
                    match self
                        .poll_task_with(token, &task_id, poll_interval, poll_attempts)
                        .await
                    {
                        Ok(url) => {
                            return Ok(GeneratedAsset {
                                url,
                                model: model.clone(),
                                usage: "image_count=1".to_owned(),
                            });
                        }
                        Err(failure) => last = Some((model.clone(), failure)),
                    }
                }
                Ok((status, text)) => {
                    let failure = GenerationFailure::classify(status, &text);
                    last = Some((model.clone(), failure));
                    if failure == GenerationFailure::Credential {
                        return Err((model.clone(), failure));
                    }
                }
                Err(_) => last = Some((model.clone(), GenerationFailure::Transient)),
            }
        }
        Err(last.unwrap_or_else(|| (String::new(), GenerationFailure::Invalid)))
    }

    /// 生成一段转场视频（图生视频：首帧为参考图 URL）。
    pub async fn generate_video(
        &self,
        token: &str,
        prompt: &str,
        first_frame_url: &str,
    ) -> Result<GeneratedAsset, (String, GenerationFailure)> {
        self.generate_video_with_poll(
            token,
            prompt,
            first_frame_url,
            Duration::from_secs(15),
            40,
        )
        .await
    }

    /// 同上，但轮询间隔与次数可注入。
    pub async fn generate_video_with_poll(
        &self,
        token: &str,
        prompt: &str,
        first_frame_url: &str,
        poll_interval: Duration,
        poll_attempts: u32,
    ) -> Result<GeneratedAsset, (String, GenerationFailure)> {
        let mut last: Option<(String, GenerationFailure)> = None;
        for model in &self.config.video_models {
            let body = video_body(model, prompt, first_frame_url, &self.config);
            match self
                .transport
                .post(
                    &format!("{}{VIDEO_PATH}", self.config.base_url),
                    token,
                    true,
                    &body,
                )
                .await
            {
                Ok((200, text)) => {
                    let Some(task_id) = parse_task_id(&text) else {
                        last = Some((model.clone(), GenerationFailure::Invalid));
                        continue;
                    };
                    match self
                        .poll_task_with(token, &task_id, poll_interval, poll_attempts)
                        .await
                    {
                        Ok(url) => {
                            return Ok(GeneratedAsset {
                                url,
                                model: model.clone(),
                                usage: format!(
                                    "video_count=1 duration={}s resolution={}",
                                    self.config.video_duration_seconds,
                                    self.config.video_resolution
                                ),
                            });
                        }
                        Err(failure) => last = Some((model.clone(), failure)),
                    }
                }
                Ok((status, text)) => {
                    let failure = GenerationFailure::classify(status, &text);
                    last = Some((model.clone(), failure));
                    if failure == GenerationFailure::Credential {
                        return Err((model.clone(), failure));
                    }
                }
                Err(_) => last = Some((model.clone(), GenerationFailure::Transient)),
            }
        }
        Err(last.unwrap_or_else(|| (String::new(), GenerationFailure::Invalid)))
    }

    /// 轮询异步任务直到终态。返回结果资源 URL。
    ///
    /// 上游建议轮询间隔 15 秒、查询接口 RPS 20；这里可注入以便测试。
    pub async fn poll_task_with(
        &self,
        token: &str,
        task_id: &str,
        interval: Duration,
        max_attempts: u32,
    ) -> Result<String, GenerationFailure> {
        for _ in 0..max_attempts {
            tokio::time::sleep(interval).await;
            let url = format!("{}/tasks/{task_id}", self.config.base_url);
            let Ok((status, text)) = self.transport.get(&url, token).await else {
                continue;
            };
            if status != 200 {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            let state = value["output"]["task_status"].as_str().unwrap_or("");
            match state {
                "SUCCEEDED" => {
                    if let Some(url) = extract_result_url(&value) {
                        return Ok(url);
                    }
                    return Err(GenerationFailure::Invalid);
                }
                "FAILED" | "CANCELED" | "UNKNOWN" => return Err(GenerationFailure::Invalid),
                _ => {}
            }
        }
        Err(GenerationFailure::Transient)
    }
}

/// 同步多模态图像请求体（实测形状）。
pub fn multimodal_body(model: &str, prompt: &str) -> Value {
    serde_json::json!({
        "model": model,
        "input": {
            "messages": [
                { "role": "user", "content": [ { "text": prompt } ] }
            ]
        },
        "parameters": { "size": "1024*1024", "n": 1 }
    })
}

/// 异步 text2image 请求体（文档与实测形状）。
pub fn text2image_body(model: &str, prompt: &str) -> Value {
    serde_json::json!({
        "model": model,
        "input": { "prompt": prompt },
        "parameters": { "size": "1024*1024", "n": 1 }
    })
}

/// 图生视频请求体（首帧模式；`resolution`/`duration`/`audio` 全部显式传）。
pub fn video_body(
    model: &str,
    prompt: &str,
    first_frame_url: &str,
    config: &GenerationConfig,
) -> Value {
    serde_json::json!({
        "model": model,
        "input": {
            "prompt": prompt,
            "media": [ { "type": "first_frame", "url": first_frame_url } ]
        },
        "parameters": {
            "resolution": config.video_resolution,
            "ratio": "adaptive",
            "duration": config.video_duration_seconds,
            "audio": config.video_audio,
            "prompt_extend": false
        }
    })
}

/// 从同步多模态响应里取图片 URL 与 usage。
fn parse_multimodal_image(text: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(text).ok()?;
    let content = value["output"]["choices"][0]["message"]["content"].as_array()?;
    let url = content
        .iter()
        .find_map(|item| item["image"].as_str())
        .map(str::to_owned)?;
    Some((url, value["usage"].to_string()))
}

fn parse_task_id(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    value["output"]["task_id"].as_str().map(str::to_owned)
}

/// 结果资源 URL 的位置在图像与视频两条链上**不一致**，因此两种都认。
fn extract_result_url(value: &Value) -> Option<String> {
    if let Some(url) = value["output"]["video_url"].as_str() {
        return Some(url.to_owned());
    }
    if let Some(url) = value["output"]["results"][0]["url"].as_str() {
        return Some(url.to_owned());
    }
    value["output"]["results"][0]["video_url"]
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 假传输：按调用序列返回预设响应，并记录 (url, asynchronous)。
    struct FakeTransport {
        posts: Mutex<Vec<(String, bool, Value)>>,
        gets: Mutex<Vec<String>>,
        post_replies: Mutex<Vec<(u16, String)>>,
        get_replies: Mutex<Vec<(u16, String)>>,
    }

    impl FakeTransport {
        fn new(post_replies: Vec<(u16, String)>, get_replies: Vec<(u16, String)>) -> Self {
            Self {
                posts: Mutex::new(Vec::new()),
                gets: Mutex::new(Vec::new()),
                post_replies: Mutex::new(post_replies),
                get_replies: Mutex::new(get_replies),
            }
        }
    }

    #[async_trait]
    impl GenerationTransport for FakeTransport {
        async fn post(
            &self,
            url: &str,
            _token: &str,
            asynchronous: bool,
            body: &Value,
        ) -> Result<(u16, String), String> {
            self.posts
                .lock()
                .unwrap()
                .push((url.to_owned(), asynchronous, body.clone()));
            let mut replies = self.post_replies.lock().unwrap();
            if replies.is_empty() {
                return Err("no more canned replies".to_owned());
            }
            Ok(replies.remove(0))
        }

        async fn get(&self, url: &str, _token: &str) -> Result<(u16, String), String> {
            self.gets.lock().unwrap().push(url.to_owned());
            let mut replies = self.get_replies.lock().unwrap();
            if replies.is_empty() {
                return Err("no more canned replies".to_owned());
            }
            Ok(replies.remove(0))
        }
    }

    fn image_ok(url: &str) -> (u16, String) {
        (
            200,
            serde_json::json!({
                "output": { "choices": [ { "finish_reason": "stop",
                    "message": { "content": [ { "image": url, "type": "image" } ], "role": "assistant" } } ] },
                "usage": { "image_count": 1 }
            })
            .to_string(),
        )
    }

    fn task_ok() -> (u16, String) {
        (
            200,
            serde_json::json!({ "output": { "task_id": "t-1", "task_status": "PENDING" } }).to_string(),
        )
    }

    fn succeeded_video(url: &str) -> (u16, String) {
        (
            200,
            serde_json::json!({ "output": { "task_status": "SUCCEEDED", "video_url": url } })
                .to_string(),
        )
    }

    fn succeeded_image(url: &str) -> (u16, String) {
        (
            200,
            serde_json::json!({ "output": { "task_status": "SUCCEEDED",
                "results": [ { "url": url } ] }, "usage": { "image_count": 1 } })
            .to_string(),
        )
    }

    #[test]
    fn settings_partial_and_invalid_values_fall_back_instead_of_breaking() {
        // 合法值按序保留
        let config = GenerationConfig::from_parts(
            "a-model, b-model ,a-model",
            "v-model",
            "480P",
            2,
        );
        assert_eq!(config.image_models, vec!["a-model", "b-model"], "保序去重");
        assert_eq!(config.video_models, vec!["v-model"]);
        assert_eq!(config.video_resolution, "480P");
        assert_eq!(config.video_duration_seconds, 2);

        // 全空 / 非法 → 回落到默认，绝不生成空链或发出越界参数
        let broken = GenerationConfig::from_parts("", "", "8K", 999);
        let defaults = GenerationConfig::default();
        assert_eq!(broken.image_models, defaults.image_models);
        assert_eq!(broken.video_models, defaults.video_models);
        assert_eq!(broken.video_resolution, "480P", "未知档位必须回落，否则按 1080P 计费");
        assert_eq!(broken.video_duration_seconds, 2, "越界时长必须回落");

        // 下限与上限都在范围内
        assert_eq!(
            GenerationConfig::from_parts("m", "v", "720p", 30).video_duration_seconds,
            30
        );
        assert_eq!(
            GenerationConfig::from_parts("m", "v", "720p", 1).video_resolution,
            "720P"
        );
    }

    /// 分类判据必须与实测响应体一致——否则会像这次一样把"路径错"误读成"没额度"。
    #[test]
    fn classifies_upstream_failures_from_real_response_shapes() {
        // 实测：免费额度耗尽
        assert_eq!(
            GenerationFailure::classify(
                403,
                r#"{"code":"AllocationQuota.FreeTierOnly","message":"Free quota exhausted."}"#
            ),
            GenerationFailure::QuotaExhausted
        );
        // 实测：仅同步路径收到了异步头
        assert_eq!(
            GenerationFailure::classify(
                403,
                r#"{"code":"AccessDenied","message":"current user api does not support asynchronous calls"}"#
            ),
            GenerationFailure::ModeMismatch
        );
        // 实测：模型不在该路径下（**不是**配额问题）
        assert_eq!(
            GenerationFailure::classify(400, r#"{"code":"InvalidParameter","message":"url error, please check url！"}"#),
            GenerationFailure::WrongPath
        );
        assert_eq!(
            GenerationFailure::classify(400, r#"{"code":"InvalidParameter","message":"Model not exist."}"#),
            GenerationFailure::ModelUnavailable
        );
        assert_eq!(
            GenerationFailure::classify(401, r#"{"code":"InvalidApiKey"}"#),
            GenerationFailure::Credential
        );
    }

    #[test]
    fn quota_and_missing_model_switch_model_but_credential_does_not() {
        assert!(GenerationFailure::QuotaExhausted.should_try_next_model());
        assert!(GenerationFailure::ModelUnavailable.should_try_next_model());
        assert!(GenerationFailure::ModeMismatch.should_try_next_model());
        assert!(!GenerationFailure::Credential.should_try_next_model());
        assert!(!GenerationFailure::Transient.should_try_next_model());
    }

    #[test]
    fn image_body_targets_the_verified_sync_multimodal_shape() {
        let body = multimodal_body("qwen-image-3.0", "一只猫");
        assert_eq!(body["model"], "qwen-image-3.0");
        assert_eq!(body["input"]["messages"][0]["role"], "user");
        assert_eq!(body["input"]["messages"][0]["content"][0]["text"], "一只猫");
        // 同步路径**不允许**出现 media/url 之类字段。
        assert!(body["input"].get("prompt").is_none());
    }

    #[test]
    fn video_body_pins_resolution_duration_and_audio() {
        let config = GenerationConfig::default();
        let body = video_body("wan3.0-video-prime", "推近", "https://x/a.png", &config);
        // 上游默认是 1080P / 有声 / 5 秒：三项都必须显式覆盖，否则会多花钱或语义错。
        assert_eq!(body["parameters"]["resolution"], "480P");
        assert_eq!(body["parameters"]["duration"], 2);
        assert_eq!(body["parameters"]["audio"], false);
        assert_eq!(body["input"]["media"][0]["type"], "first_frame");
        assert_eq!(body["input"]["media"][0]["url"], "https://x/a.png");
    }

    #[tokio::test]
    async fn image_uses_sync_multimodal_first_and_needs_no_async_header() {
        let transport = FakeTransport::new(vec![image_ok("https://img/1.png")], vec![]);
        let client = GenerationClient::new(transport, GenerationConfig::default());
        let asset = client.generate_image("tok", "一只猫").await.expect("ok");
        assert_eq!(asset.url, "https://img/1.png");
        assert_eq!(asset.model, "qwen-image-3.0");
        let posts = client.transport.posts.lock().unwrap();
        assert_eq!(posts.len(), 1, "首次成功不应再试第二条路径");
        assert!(!posts[0].1, "同步路径必须**不带**异步头");
        assert!(posts[0].0.ends_with(MULTIMODAL_PATH));
    }

    /// 关键回归：新模型在旧路径报 `url error` 时，必须**自动换到异步路径**，
    /// 而不是当成"模型不可用"直接放弃。
    #[tokio::test]
    async fn image_falls_back_to_async_path_on_wrong_path() {
        let transport = FakeTransport::new(
            vec![
                (400, r#"{"code":"InvalidParameter","message":"url error, please check url！"}"#.to_owned()),
                task_ok(),
                succeeded_image("https://img/async.png"),
            ],
            vec![succeeded_image("https://img/async.png")],
        );
        let client = GenerationClient::new(transport, GenerationConfig::default());
        // 允许极短轮询间隔以免测试变慢。
        let asset = client
            .generate_image_with_poll("tok", "一只猫", Duration::from_millis(1), 3)
            .await
            .expect("should fall back and succeed");
        assert_eq!(asset.url, "https://img/async.png");
        let posts = client.transport.posts.lock().unwrap();
        assert_eq!(posts.len(), 2);
        assert!(!posts[0].1, "第一次：同步多模态");
        assert!(posts[1].1, "第二次：异步 text2image 必须带异步头");
    }

    #[tokio::test]
    async fn image_moves_to_next_model_when_quota_exhausted() {
        let transport = FakeTransport::new(
            vec![
                (403, r#"{"code":"AllocationQuota.FreeTierOnly","message":"Free quota exhausted."}"#.to_owned()),
                image_ok("https://img/second-model.png"),
            ],
            vec![],
        );
        let client = GenerationClient::new(transport, GenerationConfig::default());
        let asset = client.generate_image("tok", "一只猫").await.expect("ok");
        assert_eq!(asset.model, "qwen-image-2.1-pro", "应换到链上的下一个模型");
        assert_eq!(asset.url, "https://img/second-model.png");
    }

    #[tokio::test]
    async fn credential_failure_stops_immediately_without_burning_the_chain() {
        let transport = FakeTransport::new(
            vec![(401, r#"{"code":"InvalidApiKey"}"#.to_owned())],
            vec![],
        );
        let client = GenerationClient::new(transport, GenerationConfig::default());
        let error = client.generate_image("tok", "一只猫").await.unwrap_err();
        assert_eq!(error.1, GenerationFailure::Credential);
        assert_eq!(
            client.transport.posts.lock().unwrap().len(),
            1,
            "凭据问题换模型没有意义，必须立刻停"
        );
    }

    #[tokio::test]
    async fn video_submits_async_then_polls_to_success() {
        let transport = FakeTransport::new(
            vec![task_ok()],
            vec![succeeded_video("https://vid/1.mp4")],
        );
        let client = GenerationClient::new(transport, GenerationConfig::default());
        let asset = client
            .generate_video_with_poll("tok", "推近", "https://x/a.png", Duration::from_millis(1), 3)
            .await
            .expect("ok");
        assert_eq!(asset.url, "https://vid/1.mp4");
        assert_eq!(asset.model, "wan3.0-video-prime");
        let posts = client.transport.posts.lock().unwrap();
        assert_eq!(posts.len(), 1);
        assert!(posts[0].1, "视频路径必须带异步头");
        assert!(posts[0].0.ends_with(VIDEO_PATH));
    }

    /// 视频结果 URL 与图像不在同一字段：两种都要认，否则成功也拿不到产物。
    #[test]
    fn extracts_result_url_from_either_image_or_video_shape() {
        let video = serde_json::json!({ "output": { "video_url": "https://v/1.mp4" } });
        assert_eq!(extract_result_url(&video).as_deref(), Some("https://v/1.mp4"));
        let image = serde_json::json!({ "output": { "results": [ { "url": "https://i/1.png" } ] } });
        assert_eq!(extract_result_url(&image).as_deref(), Some("https://i/1.png"));
    }
}
