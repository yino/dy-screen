use dy_screen_app_lib::domain::{
    CreateStreamerRequest, MAX_STREAMER_TAGS, Streamer, StreamerPromptContext, StreamerPromptTag,
    StreamerSourceKind, StreamerTagInput, normalize_streamer_tags,
};

fn tag(name: &str, prompt_guidance: Option<&str>) -> StreamerTagInput {
    StreamerTagInput {
        name: name.to_owned(),
        prompt_guidance: prompt_guidance.map(str::to_owned),
    }
}

#[test]
fn 标签名称和指导会去除首尾空白并保留可见大小写() {
    let normalized = normalize_streamer_tags(&[
        tag("  带货  ", Some("  重点提取商品卖点  ")),
        tag("Funny", Some("   ")),
    ])
    .expect("有效标签应通过校验");

    assert_eq!(normalized[0].name, "带货");
    assert_eq!(
        normalized[0].prompt_guidance.as_deref(),
        Some("重点提取商品卖点")
    );
    assert_eq!(normalized[1].name, "Funny");
    assert_eq!(normalized[1].prompt_guidance, None);
}

#[test]
fn 标签允许名称和指导的最大长度() {
    let name = "标".repeat(24);
    let guidance = "句".repeat(500);

    let normalized =
        normalize_streamer_tags(&[tag(&name, Some(&guidance))]).expect("边界长度应通过校验");

    assert_eq!(normalized[0].name.chars().count(), 24);
    assert_eq!(
        normalized[0]
            .prompt_guidance
            .as_deref()
            .expect("指导应保留")
            .chars()
            .count(),
        500
    );
}

#[test]
fn 标签拒绝空名称和超过长度限制的内容() {
    let empty = normalize_streamer_tags(&[tag("  ", None)]).expect_err("空名称必须失败");
    assert_eq!(empty.code(), "streamer_tag_name_required");
    assert_eq!(empty.field(), "tags");

    let long_name =
        normalize_streamer_tags(&[tag(&"标".repeat(25), None)]).expect_err("过长名称必须失败");
    assert_eq!(long_name.code(), "streamer_tag_name_too_long");

    let long_guidance = normalize_streamer_tags(&[tag("带货", Some(&"句".repeat(501)))])
        .expect_err("过长指导必须失败");
    assert_eq!(long_guidance.code(), "streamer_tag_guidance_too_long");
}

#[test]
fn 标签拒绝控制字符且安全错误不回显非法内容() {
    let error = normalize_streamer_tags(&[tag(
        "带货\n不可回显的完整内容",
        Some("指导\u{0000}也不能回显"),
    )])
    .expect_err("控制字符必须失败");

    assert_eq!(error.code(), "streamer_tag_control_character");
    assert_eq!(error.field(), "tags");
    assert!(!error.to_string().contains("不可回显"));
    assert!(!error.to_string().contains("也不能回显"));
}

#[test]
fn 每个主播最多保存十个标签() {
    let valid = (0..MAX_STREAMER_TAGS)
        .map(|index| tag(&format!("标签{index}"), None))
        .collect::<Vec<_>>();
    assert!(normalize_streamer_tags(&valid).is_ok());

    let invalid = (0..=MAX_STREAMER_TAGS)
        .map(|index| tag(&format!("标签{index}"), None))
        .collect::<Vec<_>>();
    let error = normalize_streamer_tags(&invalid).expect_err("超过十个必须失败");
    assert_eq!(error.code(), "streamer_tags_too_many");
}

#[test]
fn 同一主播标签名称按去空白和大小写无关规则去重() {
    let error = normalize_streamer_tags(&[tag(" Funny ", None), tag("funny", None)])
        .expect_err("规范化后的重复名称必须失败");

    assert_eq!(error.code(), "streamer_tag_duplicate");
}

#[test]
fn 标签请求和未来上下文使用稳定驼峰序列化且不包含模型字段() {
    let request = CreateStreamerRequest {
        name: "示例主播".to_owned(),
        source_url: "https://live.douyin.com/123".to_owned(),
        monitor_enabled: true,
        tags: vec![tag("带货", Some("重点提取商品卖点"))],
    };
    let request_json = serde_json::to_value(request).expect("请求应可序列化");
    assert_eq!(
        request_json["tags"][0]["promptGuidance"],
        "重点提取商品卖点"
    );

    let context = StreamerPromptContext {
        streamer_id: 7,
        streamer_name: "示例主播".to_owned(),
        tags: vec![StreamerPromptTag {
            name: "带货".to_owned(),
            prompt_guidance: Some("重点提取商品卖点".to_owned()),
            priority: 0,
        }],
    };
    let context_json = serde_json::to_value(context).expect("上下文应可序列化");
    assert_eq!(context_json["streamerId"], 7);
    assert_eq!(context_json["streamerName"], "示例主播");
    assert_eq!(
        context_json["tags"][0]["promptGuidance"],
        "重点提取商品卖点"
    );
    assert!(context_json.get("prompt").is_none());
    assert!(context_json.get("model").is_none());
    assert!(context_json.get("messages").is_none());
}

#[test]
fn 无标签请求和上下文始终输出空数组() {
    let request: CreateStreamerRequest = serde_json::from_value(serde_json::json!({
        "name": "旧客户端主播",
        "sourceUrl": "https://live.douyin.com/123",
        "monitorEnabled": true
    }))
    .expect("旧客户端缺少 tags 时应兼容为空数组");
    assert!(request.tags.is_empty());

    let context = StreamerPromptContext {
        streamer_id: 8,
        streamer_name: "无标签主播".to_owned(),
        tags: Vec::new(),
    };
    let value = serde_json::to_value(context).expect("上下文应可序列化");
    assert_eq!(value["tags"], serde_json::json!([]));

    let streamer = Streamer {
        id: 8,
        name: "无标签主播".to_owned(),
        source_kind: StreamerSourceKind::Room,
        source_url: "https://live.douyin.com/123".to_owned(),
        profile_sec_uid: None,
        web_rid: Some("123".to_owned()),
        room_url: Some("https://live.douyin.com/123".to_owned()),
        room_id: Some("456".to_owned()),
        monitor_enabled: true,
        archived: false,
        live_status: "offline".to_owned(),
        monitor_status: "waiting".to_owned(),
        last_checked_at: None,
        last_error: None,
        current_video_count: 0,
        history_video_count: 0,
        tags: Vec::new(),
    };
    let value = serde_json::to_value(streamer).expect("主播 DTO 应可序列化");
    assert_eq!(value["tags"], serde_json::json!([]));
}
