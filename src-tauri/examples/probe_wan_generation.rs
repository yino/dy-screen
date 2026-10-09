//! 万相/千问图像·视频生成候选探测：逐个真实提交，给出**可用性裁决表**。
//!
//! 目的：把"哪些模型有免费额度、哪些要走付费、哪些不可用"变成可核对的证据，
//! 而不是靠猜。裁决依据是上游的真实响应：
//! - `SUCCEEDED` → 可用（若在免费额度内则不产生费用）
//! - `AllocationQuota.FreeTierOnly` → 免费额度已耗尽，需付费或改开关
//! - `InvalidApiKey` → 凭据不被接受
//! - `InvalidParameter` / `url error` → 路径或请求形状不对，**与配额无关**
//! - `Model not exist` → 该模型在当前地域/账号不可用
//!
//! 运行：
//! ```bash
//! DY_SCREEN_DATA_DIR=/tmp/db-copy DY_SCREEN_API_BASE_URL='http://<api>/api/' \
//!   cargo run --offline --manifest-path src-tauri/Cargo.toml --example probe_wan_generation
//! ```
//!
//! 安全：不打印 token 明文；资源 URL 只打印前缀。

use std::path::PathBuf;
use std::sync::Arc;

use dy_screen_app_lib::activation::ActivationService;
use dy_screen_app_lib::ai::{
    ActivationLlmTokenSource, AiRepository, AssetExpectation, FfprobeMediaProbe, GenerationClient,
    GenerationConfig, ModelCallRuntime, ReqwestTransport, data_uri, download_and_validate,
};
use dy_screen_app_lib::api::ApiClient;
use dy_screen_app_lib::database::Database;

/// 北京地域的裸域名（官方 Postman 指南确认）。
const BASE: &str = "https://dashscope.aliyuncs.com/api/v1";
const IMAGE_PATH: &str = "/services/aigc/text2image/image-synthesis";
const VIDEO_PATH: &str = "/services/aigc/video-generation/video-synthesis";
/// 新一代统一多模态生成路径：较新的图像模型可能只在这里提供。
const MULTIMODAL_PATH: &str = "/services/aigc/multimodal-generation/generation";

/// 一次探测的裁决。
struct Verdict {
    label: String,
    outcome: String,
    detail: String,
}

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
        repository.clone(),
        tokens,
        chrono::Utc::now().timestamp_millis(),
    )?);
    let lease = model_call
        .acquire_token(chrono::Utc::now().timestamp())
        .await?;
    let head: String = lease.token.chars().take(6).collect();
    println!(
        "凭据：provider={} 前缀={head}… 长度={}\n",
        lease.provider,
        lease.token.len()
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    // 用户给定的优先级：先用有免费额度的，用完再走付费备选。
    let plans: Vec<(&str, &str, serde_json::Value)> = vec![
        // ── 免费额度候选：image ──
        (
            "qwen-image-3.0",
            IMAGE_PATH,
            image_body("qwen-image-3.0"),
        ),
        (
            "qwen-image-2.1-pro",
            IMAGE_PATH,
            image_body("qwen-image-2.1-pro"),
        ),
        (
            "qwen-mt-image-2.0",
            IMAGE_PATH,
            image_body("qwen-mt-image-2.0"),
        ),
        // ── 免费额度候选：video ──
        (
            "wan3.0-video-prime",
            VIDEO_PATH,
            video_body("wan3.0-video-prime"),
        ),
        // ── 付费备选：image ──
        ("qwen-image-plus", IMAGE_PATH, image_body("qwen-image-plus")),
        ("qwen-image", IMAGE_PATH, image_body("qwen-image")),
        ("wan2.6-t2i", IMAGE_PATH, image_body("wan2.6-t2i")),
        // ── 付费备选：video（480P）──
        (
            "wan3.0-video",
            VIDEO_PATH,
            video_body("wan3.0-video"),
        ),
    ];

    let mut verdicts = Vec::new();
    for (label, path, body) in plans {
        let is_video = path == VIDEO_PATH;
        verdicts.push(run_one(&client, &lease.token, label, path, body, is_video).await);
    }

    // 第二轮：那 4 个在 `text2image/image-synthesis` 上统一报 `url error` 的模型，
    // 换到全新的统一多模态生成路径上再试一次。报错一致说明它们**不在旧路径下**，
    // 而不是"配额用完"或"参数不对"。
    println!("\n\n──────── 第二轮：统一多模态生成路径 ────────");
    let multimodal: Vec<(&str, serde_json::Value)> = vec![
        ("qwen-image-3.0", multimodal_body("qwen-image-3.0")),
        (
            "qwen-image-2.1-pro",
            multimodal_body("qwen-image-2.1-pro"),
        ),
        (
            "qwen-mt-image-2.0",
            multimodal_body("qwen-mt-image-2.0"),
        ),
        ("wan2.6-t2i", multimodal_body("wan2.6-t2i")),
    ];
    for (label, body) in multimodal {
        verdicts.push(
            run_one(
                &client,
                &lease.token,
                &format!("{label} ·多模态路径"),
                MULTIMODAL_PATH,
                body,
                false,
            )
            .await,
        );
    }

    // 第三轮：对"仅支持同步"的模型改用**无异步头**的同步调用。
    println!("\n\n──────── 第三轮：多模态路径 · 同步调用（不带异步头）────────");
    let sync_candidates: Vec<(&str, serde_json::Value)> = vec![
        ("qwen-image-3.0", multimodal_body("qwen-image-3.0")),
        (
            "qwen-image-2.1-pro",
            multimodal_body("qwen-image-2.1-pro"),
        ),
        ("wan2.6-t2i", multimodal_body("wan2.6-t2i")),
    ];
    for (label, body) in sync_candidates {
        let url = format!("{BASE}{MULTIMODAL_PATH}");
        println!("\n=== {label} · 同步 ===");
        match post_sync(&client, &lease.token, &url, &body).await {
            Ok((status, text)) => {
                println!("HTTP {status}");
                println!("  {}", shorten(&text, 400));
                let outcome = if status == 200 {
                    "✅ 可用（同步）"
                } else if text.contains("FreeTierOnly") {
                    "❌ 配额拒绝"
                } else if text.contains("url error") {
                    "⚠️ 路径/形状不对"
                } else {
                    "⚠️ 被拒"
                };
                verdicts.push(verdict(
                    &format!("{label} ·同步"),
                    outcome,
                    shorten(&text, 120),
                ));
            }
            Err(error) => {
                println!("❌ 传输失败：{error}");
                verdicts.push(verdict(
                    &format!("{label} ·同步"),
                    "传输失败",
                    error.to_string(),
                ));
            }
        }
    }

    // ── 第四轮：用**产品自身的客户端**真实走一遍完整链路 ──
    // 生成参考图 → 下载并校验 → base64 首帧 → 图生视频 → 下载并校验。
    // 这一步验证的是"新写的下载与校验模块能否对着真实上游工作"。
    println!("\n\n──────── 第四轮：真实生成 + 下载校验 ────────");
    let verify_dir = std::env::temp_dir().join("dy-screen-gen-verify");
    let _ = std::fs::remove_dir_all(&verify_dir);
    let client = GenerationClient::new(ReqwestTransport::default(), GenerationConfig::default());
    let probe = FfprobeMediaProbe {
        ffprobe_path: PathBuf::from("ffprobe"),
    };

    match client
        .generate_image(&lease.token, "一只在草地上奔跑的橘猫，写实摄影，自然光")
        .await
    {
        Err((model, failure)) => println!("参考图生成失败：{model} {failure:?}"),
        Ok(image) => {
            println!("参考图模型={} task={:?}", image.model, image.task_id);
            let reference = verify_dir.join("reference.png");
            match download_and_validate(
                client.transport_ref(),
                &probe,
                &image.url,
                &reference,
                AssetExpectation::image(),
            )
            .await
            {
                Err(rejection) => println!("参考图校验未通过：{} {rejection:?}", rejection.code()),
                Ok(asset) => {
                    println!(
                        "  ✅ 参考图校验通过：{}x{} sha256={}… {} 字节",
                        asset.media.width, asset.media.height, &asset.sha256[..12], asset.bytes
                    );
                    let Ok(bytes) = std::fs::read(&asset.path) else {
                        println!("  读取参考图失败，跳过视频");
                        return Ok(());
                    };
                    let first_frame = data_uri("image/png", &bytes);
                    println!("  首帧以 base64 data URI 传递，长度 {} 字符", first_frame.len());

                    match client
                        .generate_video(&lease.token, "镜头缓慢推近，温暖自然光", &first_frame)
                        .await
                    {
                        Err((model, failure)) => {
                            println!("视频生成失败：{model} {failure:?}")
                        }
                        Ok(video) => {
                            println!("视频模型={} task={:?}", video.model, video.task_id);
                            let target = verify_dir.join("transition.mp4");
                            match download_and_validate(
                                client.transport_ref(),
                                &probe,
                                &video.url,
                                &target,
                                AssetExpectation::video(2_000),
                            )
                            .await
                            {
                                Err(rejection) => println!(
                                    "  ❌ 视频校验未通过：{} {rejection:?}",
                                    rejection.code()
                                ),
                                Ok(asset) => println!(
                                    "  ✅ 视频校验通过：{}x{} {}fps {}ms sha256={}… {} 字节",
                                    asset.media.width,
                                    asset.media.height,
                                    asset.media.fps,
                                    asset.media.duration_ms,
                                    &asset.sha256[..12],
                                    asset.bytes
                                ),
                            }
                        }
                    }
                }
            }
        }
    }

    println!("\n\n════════ 裁决表 ════════");
    println!("{:<22} {:<28} {}", "模型", "结果", "说明");
    for verdict in &verdicts {
        println!(
            "{:<22} {:<28} {}",
            verdict.label, verdict.outcome, verdict.detail
        );
    }
    Ok(())
}

fn image_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "input": { "prompt": "一只在草地上奔跑的橘猫，写实摄影风格，自然光" },
        "parameters": { "size": "1024*1024", "n": 1 }
    })
}

/// 统一多模态生成路径的请求体：`input.messages` + content 数组。
fn multimodal_body(model: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "input": {
            "messages": [
                { "role": "user", "content": [ { "text": "一只在草地上奔跑的橘猫，写实摄影风格，自然光" } ] }
            ]
        },
        "parameters": { "size": "1024*1024", "n": 1 }
    })
}

fn video_body(model: &str) -> serde_json::Value {
    // 480P + 2 秒（API 下限）+ 关音频（我们的转场是无音频桥接）。
    serde_json::json!({
        "model": model,
        "input": { "prompt": "镜头缓慢推近，温暖的自然光，电影质感" },
        "parameters": {
            "resolution": "480P",
            "ratio": "adaptive",
            "duration": 2,
            "audio": false
        }
    })
}

async fn run_one(
    client: &reqwest::Client,
    token: &str,
    label: &str,
    path: &str,
    body: serde_json::Value,
    is_video: bool,
) -> Verdict {
    let url = format!("{BASE}{path}");
    println!("\n=== {label} ===");
    let (status, text) = match post(client, token, &url, &body).await {
        Ok(value) => value,
        Err(error) => {
            println!("❌ 传输失败：{error}");
            return verdict(label, "传输失败", error.to_string());
        }
    };
    println!("提交 → HTTP {status}");
    println!("  {}", shorten(&text, 320));

    if status == 403 {
        let hint = if text.contains("FreeTierOnly") {
            "免费额度已耗尽：需付费，或在控制台关闭「仅使用免费额度」"
        } else {
            "403：权限或额度被拒"
        };
        return verdict(label, "❌ 配额拒绝", hint.to_owned());
    }
    if status == 401 {
        return verdict(label, "❌ 凭据拒绝", "InvalidApiKey：token 不被接受".to_owned());
    }
    if status != 200 {
        // 400：区分"模型不存在"与"参数/路径不对"——后者与配额无关。
        let detail = if text.contains("not exist") || text.contains("not found") {
            "模型不在当前地域/账号"
        } else if text.contains("url error") {
            "路径或请求形状不对（未走到配额检查）"
        } else {
            "参数被拒（未走到配额检查）"
        };
        return verdict(label, "⚠️ 请求被拒", detail.to_owned());
    }

    let parsed: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return verdict(label, "⚠️ 响应非 JSON", String::new()),
    };
    let Some(task_id) = parsed["output"]["task_id"].as_str() else {
        return verdict(label, "⚠️ 缺少 task_id", String::new());
    };

    let budget = if is_video { 90_u32 } else { 40_u32 };
    println!("  task_id 已获取，轮询（间隔 3 秒，最多 {budget} 次）");
    for attempt in 1..=budget {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let poll_url = format!("{BASE}/tasks/{task_id}");
        let Ok(response) = client
            .get(&poll_url)
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
        else {
            continue;
        };
        let text = response.text().await.unwrap_or_default();
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let state = value["output"]["task_status"].as_str().unwrap_or("?");
        if attempt % 5 == 0 || state != "RUNNING" {
            println!("  [{attempt}] {state}");
        }
        match state {
            "SUCCEEDED" => {
                let usage = value["usage"].to_string();
                if let Some(url) = value["output"]["results"][0]["url"].as_str() {
                    let shown: String = url.chars().take(90).collect();
                    println!("  资源：{shown}…");
                }
                return verdict(label, "✅ 可用", format!("usage={usage}"));
            }
            "FAILED" | "CANCELED" | "UNKNOWN" => {
                println!("  {}", shorten(&text, 400));
                return verdict(label, "❌ 任务终态失败", state.to_owned());
            }
            _ => {}
        }
    }
    verdict(label, "⏱️ 轮询超时", "仍在处理中".to_owned())
}

fn verdict(label: &str, outcome: &str, detail: String) -> Verdict {
    Verdict {
        label: label.to_owned(),
        outcome: outcome.to_owned(),
        detail,
    }
}

/// 同步调用：**不带** `X-DashScope-Async`。多模态生成路径只支持同步。
async fn post_sync(
    client: &reqwest::Client,
    token: &str,
    url: &str,
    body: &serde_json::Value,
) -> Result<(u16, String), reqwest::Error> {
    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .json(body)
        .send()
        .await?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    Ok((status, text))
}

async fn post(
    client: &reqwest::Client,
    token: &str,
    url: &str,
    body: &serde_json::Value,
) -> Result<(u16, String), reqwest::Error> {
    let response = client
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("X-DashScope-Async", "enable")
        .json(body)
        .send()
        .await?;
    let status = response.status().as_u16();
    let text = response.text().await.unwrap_or_default();
    Ok((status, text))
}

fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    format!("{}…", text.chars().take(limit).collect::<String>())
}

fn dirs_app_data() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(dir) = std::env::var("DY_SCREEN_DATA_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join("Library/Application Support/com.yino.dyscreen"))
}
