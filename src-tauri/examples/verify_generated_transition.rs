//! 真实链路验证：**生成参考图 → 下载校验 → base64 首帧 → 图生视频 → 下载校验**。
//!
//! 这是"生成式转场"最靠上游的一段，也是唯一必须真实付费/消耗免费额度的一段。
//! 它用**产品自身的客户端与校验模块**（`GenerationClient` / `download_and_validate`），
//! 不复制任何业务逻辑。
//!
//! 运行：
//! ```bash
//! DY_SCREEN_DATA_DIR=/tmp/db-copy DY_SCREEN_API_BASE_URL='http://<api>/api/' \
//!   cargo run --offline --manifest-path src-tauri/Cargo.toml \
//!   --example verify_generated_transition
//! ```
//!
//! 成本：参考图 1 张（¥0.2，或免费额度）+ 转场视频 2 秒 480P（¥0.6，或免费额度）。
//! 不打印 token；资源 URL 只打印前缀。

use std::path::PathBuf;
use std::sync::Arc;

use dy_screen_app_lib::activation::ActivationService;
use dy_screen_app_lib::ai::{
    ActivationLlmTokenSource, AiRepository, AssetExpectation, FfprobeMediaProbe, GenerationClient,
    GenerationConfig, ModelCallRuntime, ReqwestTransport, data_uri, download_and_validate,
};
use dy_screen_app_lib::api::ApiClient;
use dy_screen_app_lib::database::Database;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app_data_dir = dirs_app_data()?;
    let database_path = app_data_dir.join("dy-screen.sqlite3");
    if !database_path.is_file() {
        return Err(format!("找不到数据库：{}", database_path.display()).into());
    }
    let database = Database::open(&database_path)?;
    database.migrate()?;
    let repository = AiRepository::new(database.clone());

    let api = ApiClient::from_env()?;
    let activation = ActivationService::new(database.clone(), api, &app_data_dir)?;
    let tokens = Arc::new(ActivationLlmTokenSource::new(activation));
    let model_call = Arc::new(ModelCallRuntime::bootstrap(
        repository,
        tokens,
        chrono::Utc::now().timestamp_millis(),
    )?);
    let lease = model_call
        .acquire_token(chrono::Utc::now().timestamp())
        .await?;
    println!("凭据已取得（provider={}）\n", lease.provider);

    // ffprobe 取随应用提供的路径；示例里用 PATH 上的 ffprobe。
    let probe = FfprobeMediaProbe {
        ffprobe_path: PathBuf::from("ffprobe"),
    };
    let config = GenerationConfig::default();
    println!(
        "配置：图像链={:?}\n      视频链={:?}\n      分辨率={} 时长={}s 音频={}\n",
        config.image_models,
        config.video_models,
        config.video_resolution,
        config.video_duration_seconds,
        config.video_audio
    );

    let client = GenerationClient::new(ReqwestTransport::default(), config);
    let work_dir = std::env::temp_dir().join("dy-screen-verify-transition");
    std::fs::create_dir_all(&work_dir)?;
    let video_seconds = u64::from(client.config().video_duration_seconds) * 1_000;

    // ── ① 参考图 ──
    let image = match client
        .generate_image(&lease.token, "一只在草地上奔跑的橘猫，写实摄影风格，自然光")
        .await
    {
        Ok(image) => image,
        Err((model, failure)) => {
            return Err(format!("参考图生成失败：{model} / {failure:?}").into());
        }
    };
    println!("[1/2] 参考图：模型={} task_id={:?}", image.model, image.task_id);

    let reference = work_dir.join("reference.png");
    let image_asset = match download_and_validate(
        client.transport_ref(),
        &probe,
        &image.url,
        &reference,
        AssetExpectation::image(),
    )
    .await
    {
        Ok(asset) => asset,
        Err(rejection) => return Err(format!("参考图校验未通过：{}", rejection.code()).into()),
    };
    println!(
        "      ✅ 校验通过 {}×{} {} 字节 sha256={}…",
        image_asset.media.width,
        image_asset.media.height,
        image_asset.bytes,
        &image_asset.sha256[..12]
    );

    // ── ② 图生视频（首帧走 base64，避免把本地图传到公网）──
    let bytes = std::fs::read(&image_asset.path)?;
    let first_frame = data_uri("image/png", &bytes);
    println!("      首帧 base64 长度 {} 字符", first_frame.len());

    let video = match client
        .generate_video(&lease.token, "镜头缓慢推近，温暖的自然光，电影质感", &first_frame)
        .await
    {
        Ok(video) => video,
        Err((model, failure)) => {
            return Err(format!("视频生成失败：{model} / {failure:?}").into());
        }
    };
    println!("[2/2] 转场视频：模型={} task_id={:?}", video.model, video.task_id);

    let target = work_dir.join("transition.mp4");
    let video_asset = match download_and_validate(
        client.transport_ref(),
        &probe,
        &video.url,
        &target,
        AssetExpectation::video(video_seconds),
    )
    .await
    {
        Ok(asset) => asset,
        Err(rejection) => return Err(format!("视频校验未通过：{}", rejection.code()).into()),
    };
    println!(
        "      ✅ 校验通过 {}×{} {}fps {}ms {} 字节 sha256={}…",
        video_asset.media.width,
        video_asset.media.height,
        video_asset.media.fps,
        video_asset.media.duration_ms,
        video_asset.bytes,
        &video_asset.sha256[..12]
    );
    println!("\n产物：{}", video_asset.path.display());
    Ok(())
}

fn dirs_app_data() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(dir) = std::env::var("DY_SCREEN_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join("Library/Application Support/com.yino.dyscreen"))
}
