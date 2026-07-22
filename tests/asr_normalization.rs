use dy_screen::asr::TextNormalizer;

#[test]
fn bundled_opencc_mapping_normalizes_traditional_live_text_and_money_deterministically() {
    let mapping = include_str!("../resources/asr/normalization/TSCharacters.txt");
    let normalizer = TextNormalizer::with_opencc_characters("zh-normalize-v1", mapping);
    let raw = "歡迎來到直播間, 今天價格是￥99!";
    assert_eq!(
        normalizer.normalize(raw),
        "欢迎来到直播间， 今天价格是99元！"
    );
    assert_eq!(raw, "歡迎來到直播間, 今天價格是￥99!");
    assert_eq!(normalizer.version(), "zh-normalize-v1");
}
