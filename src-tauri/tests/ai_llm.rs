use dy_screen_app_lib::ai::{
    CredentialError, CredentialStore, FakeHighlightProvider, HighlightAgentProvider,
    LlmProviderSettings, MemoryCredentialStore,
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
