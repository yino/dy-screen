use std::path::PathBuf;

use chrono::Utc;
use dy_screen::access::{
    AccessChannel, AccessClassification, AccessDiagnosticEntry, AccessMarkers, AccessNextAction,
    AccessStage,
};
use dy_screen::browser_snapshot::{BrowserPageSnapshot, parse_browser_snapshot};
use dy_screen::error::RecorderError;
use dy_screen::resolver::RoomInspection;
use dy_screen_app_lib::supervisor::MonitorLogger;

const SUPPORTED_PAGE: &str = include_str!("../../../tests/fixtures/live_room.html");
const CHALLENGE_PAGE: &str = include_str!("../../../tests/fixtures/access_restricted_room.html");
const WEB_RID: &str = "703940802949";

fn main() {
    if let Err(error) = run() {
        eprintln!("本地访问验收失败：{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let fixture = arguments.next().ok_or("缺少 fixture 名称")?;
    let log_directory = PathBuf::from(arguments.next().ok_or("缺少 JSONL 日志目录")?);
    if arguments.next().is_some() {
        return Err("参数过多；用法：access_fixture <supported|challenge> <log-directory>".into());
    }

    let (snapshot, response_bytes) = match fixture.as_str() {
        "supported" => (supported_snapshot()?, SUPPORTED_PAGE.len()),
        "challenge" => (challenge_snapshot()?, CHALLENGE_PAGE.len()),
        _ => return Err("fixture 只能是 supported 或 challenge".into()),
    };
    let markers = snapshot.markers();
    let (classification, next_action) = match parse_browser_snapshot(&snapshot) {
        Ok(RoomInspection::Live(_)) if fixture == "supported" => {
            (AccessClassification::Live, AccessNextAction::StartRecording)
        }
        Err(RecorderError::RoomAccessVerificationRequired) if fixture == "challenge" => (
            AccessClassification::VerificationRequired,
            AccessNextAction::WaitForUser,
        ),
        Ok(RoomInspection::Offline { .. }) => (
            AccessClassification::Offline,
            AccessNextAction::ScheduleNextCheck,
        ),
        Ok(RoomInspection::Live(_)) => return Err("访问验证 fixture 被错误识别为直播页".into()),
        Err(error) => return Err(error.into()),
    };

    MonitorLogger::file(log_directory.clone()).log_access(&AccessDiagnosticEntry {
        timestamp: Utc::now(),
        streamer_id: Some(1),
        web_rid: Some(WEB_RID.to_owned()),
        request_id: format!("fixture-{fixture}-1"),
        channel: AccessChannel::Browser,
        stage: AccessStage::Probe,
        classification,
        http_status: None,
        content_type: Some("text/html".to_owned()),
        response_bytes,
        markers: AccessMarkers {
            access_restricted: markers.access_restricted,
            pace_payload: markers.pace_payload,
            supported_room: classification.is_success(),
        },
        duration_ms: 1,
        failure_count: usize::from(classification == AccessClassification::VerificationRequired),
        next_action,
        next_retry_at: None,
    });
    println!(
        "{fixture} fixture 验收通过；JSONL 目录：{}",
        log_directory.display()
    );
    Ok(())
}

fn supported_snapshot() -> Result<BrowserPageSnapshot, RecorderError> {
    let start = SUPPORTED_PAGE
        .find("self.__pace_f.push")
        .ok_or(RecorderError::InvalidBrowserSnapshot)?;
    let end = SUPPORTED_PAGE[start..]
        .find("</script>")
        .map(|offset| start + offset)
        .ok_or(RecorderError::InvalidBrowserSnapshot)?;
    snapshot(false, vec![SUPPORTED_PAGE[start..end].trim().to_owned()])
}

fn challenge_snapshot() -> Result<BrowserPageSnapshot, RecorderError> {
    snapshot(true, Vec::new())
}

fn snapshot(
    access_restricted: bool,
    scripts: Vec<String>,
) -> Result<BrowserPageSnapshot, RecorderError> {
    let json = serde_json::json!({
        "url": format!("https://live.douyin.com/{WEB_RID}?verifyFp=fixture-secret"),
        "title": "本地脱敏验收页",
        "readyState": "complete",
        "markers": {
            "accessRestricted": access_restricted,
            "pacePayload": !scripts.is_empty(),
            "snapshotOverflow": false
        },
        "scripts": scripts
    })
    .to_string();
    BrowserPageSnapshot::from_json(&json)
}
