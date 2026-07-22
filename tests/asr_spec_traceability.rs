//! 防止 OpenSpec 场景、自动化证据和外部验收门禁发生静默漂移。

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn spec_files(root: &Path) -> [PathBuf; 3] {
    let change = root.join("openspec/changes/add-user-triggered-ai-asr/specs");
    [
        change.join("ai-analysis-projects/spec.md"),
        change.join("desktop-client-shell/spec.md"),
        change.join("local-speech-transcription/spec.md"),
    ]
}

fn headings(files: &[PathBuf], prefix: &str) -> BTreeSet<String> {
    files
        .iter()
        .flat_map(|path| {
            fs::read_to_string(path)
                .unwrap()
                .lines()
                .filter_map(|line| line.strip_prefix(prefix).map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn every_delta_requirement_and_scenario_has_traceable_evidence() {
    let root = root();
    let files = spec_files(&root);
    let requirements = headings(&files, "### Requirement: ");
    let scenarios = headings(&files, "#### Scenario: ");
    assert_eq!(
        requirements.len(),
        31,
        "delta Requirement 数量变化时必须复核矩阵"
    );
    assert_eq!(scenarios.len(), 85, "delta Scenario 数量变化时必须复核矩阵");

    let matrix = fs::read_to_string(root.join("docs/wiki/AI-ASR-需求测试追踪矩阵.md")).unwrap();
    for requirement in requirements {
        assert!(
            matrix.contains(&format!("`{requirement}`")),
            "追踪矩阵缺少 Requirement：{requirement}"
        );
    }
    for scenario in scenarios {
        assert!(
            matrix.contains(&format!("`{scenario}`")),
            "追踪矩阵缺少 Scenario：{scenario}"
        );
    }
}

#[test]
fn every_unfinished_openspec_task_is_explicitly_kept_incomplete() {
    let root = root();
    let tasks =
        fs::read_to_string(root.join("openspec/changes/add-user-triggered-ai-asr/tasks.md"))
            .unwrap();
    let pending = tasks
        .lines()
        .filter_map(|line| line.strip_prefix("- [ ] "))
        .map(|body| body.split_whitespace().next().unwrap().to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        pending,
        BTreeSet::from([
            "1.6".to_owned(),
            "9.2".to_owned(),
            "9.4".to_owned(),
            "9.5".to_owned(),
            "9.8".to_owned(),
            "9.9".to_owned(),
        ])
    );

    let matrix = fs::read_to_string(root.join("docs/wiki/AI-ASR-需求测试追踪矩阵.md")).unwrap();
    assert!(matrix.contains("以下六项当前状态：未完成"));
    assert!(matrix.contains("77/83"));
    for task in pending {
        assert!(
            matrix.contains(&format!("**{task}**")),
            "追踪矩阵未明确列出待完成任务 {task}"
        );
    }
}
