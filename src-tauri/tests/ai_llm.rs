use dy_screen_app_lib::ai::{
    CredentialError, CredentialStore, DEFAULT_TRANSITION_AUTO_APPLY_SCORE, FakeHighlightProvider,
    HighlightAgentProvider, LlmProviderSettings, MemoryCredentialStore, SubtitleCorrectionOutput,
    TransitionAgentOutput, TransitionScoreOutput,
};
use tokio_util::sync::CancellationToken;

#[test]
fn memory_credentials_support_missing_replace_and_clear_without_exposing_key_in_settings() {
    let store = MemoryCredentialStore::new();
    assert_eq!(store.get().unwrap(), None);
    assert!(matches!(store.set("\n"), Err(CredentialError::Invalid)));

    store.set("sk-test-secret").unwrap();
    assert_eq!(store.get().unwrap().as_deref(), Some("sk-test-secret"));
    store.set("sk-replaced").unwrap();
    assert_eq!(store.get().unwrap().as_deref(), Some("sk-replaced"));

    let settings = serde_json::to_string(&LlmProviderSettings::default()).unwrap();
    assert!(!settings.contains("sk-replaced"));
    assert!(!settings.contains("apiKey"));

    store.clear().unwrap();
    assert_eq!(store.get().unwrap(), None);
}

#[test]
fn highlight_thresholds_must_be_ordered_and_within_score_range() {
    let out_of_order = LlmProviderSettings {
        qualified_score: 80,
        excellent_score: 70,
        ..LlmProviderSettings::default()
    };
    assert!(out_of_order.validate().is_err());
    let out_of_range = LlmProviderSettings {
        qualified_score: 101,
        excellent_score: 101,
        ..LlmProviderSettings::default()
    };
    assert!(out_of_range.validate().is_err());
}

#[test]
fn transition_auto_apply_score_defaults_to_eight_and_rejects_out_of_range_values() {
    let defaults = LlmProviderSettings::default();
    assert_eq!(
        defaults.transition_auto_apply_score,
        DEFAULT_TRANSITION_AUTO_APPLY_SCORE
    );

    let legacy = serde_json::json!({
        "provider": "deepseek",
        "modelId": "deepseek-chat",
        "timeoutMs": 30_000,
        "promptVersion": "highlight-v1",
        "qualifiedScore": 70,
        "excellentScore": 80,
        "keyConfigured": false,
        "updatedAt": null
    });
    let migrated: LlmProviderSettings = serde_json::from_value(legacy).unwrap();
    assert_eq!(migrated.transition_auto_apply_score, 8);

    let invalid = LlmProviderSettings {
        transition_auto_apply_score: 11,
        ..LlmProviderSettings::default()
    };
    assert!(invalid.validate().is_err());
}

#[test]
fn provider_output_schemas_use_response_metadata_for_token_usage() {
    for schema in [
        schemars::schema_for!(TransitionAgentOutput),
        schemars::schema_for!(TransitionScoreOutput),
        schemars::schema_for!(SubtitleCorrectionOutput),
    ] {
        let schema = serde_json::to_value(schema).unwrap();
        assert!(schema.pointer("/properties/tokenUsage").is_none());
    }

    let output: TransitionAgentOutput = serde_json::from_value(serde_json::json!({
        "matches": []
    }))
    .unwrap();
    assert_eq!(output.token_usage, 0);
}

#[tokio::test]
async fn fake_provider_diagnosis_is_deterministic_and_receives_no_project_text() {
    let provider = FakeHighlightProvider::default();
    let settings = LlmProviderSettings::default();
    let result = provider
        .diagnose(&settings, "sk-test-secret", CancellationToken::new())
        .await
        .unwrap();
    assert!(result.ok);
    assert_eq!(result.category, "ok");
    assert_eq!(*provider.diagnostic_calls.lock().unwrap(), 1);
}
