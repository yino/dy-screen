use dy_screen_app_lib::ai::{AiClipSubtitle, ClipOutputDimensions, build_clip_subtitle_frames};

fn subtitle(id: i64, stable_id: &str, start_ms: u64, end_ms: u64, text: &str) -> AiClipSubtitle {
    AiClipSubtitle {
        id,
        clip_project_id: 1,
        stable_segment_id: stable_id.to_owned(),
        clip_segment_id: 10 + id,
        input_id: 1,
        original_text: text.to_owned(),
        text: text.to_owned(),
        hidden: false,
        source_start_ms: start_ms,
        source_end_ms: end_ms,
        project_start_ms: start_ms,
        project_end_ms: end_ms,
    }
}

#[test]
fn chinese_punctuation_uses_grapheme_steps_and_holds_full_text_after_eighty_five_percent() {
    let frames = build_clip_subtitle_frames(
        &[subtitle(1, "zh", 0, 1_000, "你好呀。")],
        ClipOutputDimensions {
            width: 1_920,
            height: 1_080,
        },
        1_000,
    );

    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0].visible_text, "你");
    assert_eq!(frames[0].hidden_text, "好呀。");
    assert_eq!(frames[1].project_start_ms, 425);
    assert_eq!(frames[2].project_start_ms, 850);
    assert_eq!(frames[2].project_end_ms, 1_000);
    assert_eq!(frames[2].visible_text, "你好呀。");
    assert!(frames[2].hidden_text.is_empty());
}

#[test]
fn emoji_sequence_is_not_split_and_hidden_subtitles_generate_no_frames() {
    let frames = build_clip_subtitle_frames(
        &[subtitle(1, "emoji", 0, 1_000, "A👨‍👩‍👧‍👦B!")],
        ClipOutputDimensions {
            width: 1_920,
            height: 1_080,
        },
        1_000,
    );
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[1].visible_text, "A👨‍👩‍👧‍👦");

    let mut hidden = subtitle(2, "hidden", 0, 1_000, "不会显示");
    hidden.hidden = true;
    assert!(
        build_clip_subtitle_frames(
            &[hidden],
            ClipOutputDimensions {
                width: 1_920,
                height: 1_080
            },
            1_000,
        )
        .is_empty()
    );
}

#[test]
fn portrait_long_text_is_paginated_to_two_lines_without_truncation() {
    let text = (0..80)
        .map(|index| char::from_u32('一' as u32 + (index % 20)).unwrap())
        .collect::<String>();
    let frames = build_clip_subtitle_frames(
        &[subtitle(1, "long", 0, 8_000, &text)],
        ClipOutputDimensions {
            width: 1_080,
            height: 1_920,
        },
        8_000,
    );
    let mut pages = Vec::new();
    for frame in &frames {
        if pages.last() != Some(&frame.page_text) {
            pages.push(frame.page_text.clone());
        }
        assert!(frame.page_text.lines().count() <= 2);
    }
    assert!(pages.len() > 1);
    assert_eq!(pages.join("").replace('\n', ""), text);
}

#[test]
fn later_overlapping_subtitle_wins_deterministically_and_gaps_stay_empty() {
    let frames = build_clip_subtitle_frames(
        &[
            subtitle(1, "a", 100, 1_200, "第一句"),
            subtitle(2, "b", 600, 1_000, "第二句"),
        ],
        ClipOutputDimensions {
            width: 1_920,
            height: 1_080,
        },
        1_500,
    );
    assert!(frames.iter().all(|frame| frame.project_start_ms >= 100));
    assert!(frames.iter().all(|frame| frame.project_end_ms <= 1_200));
    assert!(frames.iter().any(|frame| {
        frame.project_start_ms <= 700 && frame.project_end_ms > 700 && frame.subtitle_id == 2
    }));
    assert!(
        !frames
            .iter()
            .any(|frame| { frame.project_start_ms <= 1_300 && frame.project_end_ms > 1_300 })
    );
}
