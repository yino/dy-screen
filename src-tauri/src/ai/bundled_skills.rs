//! 随包 Skill 的读取、校验与运行指纹（步骤 2 任务 3.6）。
//!
//! 决策三 / 决策九：Profile 与 Skill **只随包**，忽略用户目录同名配置；
//! 三条既有链路（精彩分析、转场匹配、文本纠错）继续在 Rust 内以单次结构化输出调用，
//! Skill 作为随包文件由 Rust 读取与校验，并把 `id@version` 写入运行指纹——
//! **不要求 Skill 在 Harness 内执行**。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 一个随包 Skill 定义（与 `HighlightSkill` 常量等价，但可序列化、可从文件加载）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BundledSkill {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub guidance: String,
    #[serde(default)]
    pub weights: [f32; 6],
}

/// 随包 Skill 加载与校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillLoadError {
    MissingDirectory(&'static str),
    InvalidDefinition(&'static str),
    DuplicateId(&'static str),
    InvalidVersion(&'static str),
}

impl SkillLoadError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingDirectory(_) => "skill_directory_missing",
            Self::InvalidDefinition(_) => "skill_definition_invalid",
            Self::DuplicateId(_) => "skill_duplicate_id",
            Self::InvalidVersion(_) => "skill_version_invalid",
        }
    }
}

/// 读取并校验随包 Skill 目录。
///
/// - 只读取给定目录下的 `*.skill.json`；
/// - 拒绝未知字段（`deny_unknown_fields`），防止随包资产携带未声明的工具/权限；
/// - 要求 id / version 非空、id 唯一、version 形如 `\d+.\d+.\d+`。
///
/// 本函数**不接受任何用户目录输入**：调用方只传随包路径，因此"忽略用户同名配置"
/// 是结构性的，而不是靠过滤字符串实现的。
pub fn load_bundled_skills(directory: &Path) -> Result<Vec<BundledSkill>, SkillLoadError> {
    let entries = std::fs::read_dir(directory)
        .map_err(|_| SkillLoadError::MissingDirectory("随包 Skill 目录不可读"))?;
    let mut skills = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || !path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.ends_with(".skill.json"))
        {
            continue;
        }
        let bytes = std::fs::read(&path)
            .map_err(|_| SkillLoadError::InvalidDefinition("Skill 文件不可读"))?;
        let skill: BundledSkill = serde_json::from_slice(&bytes)
            .map_err(|_| SkillLoadError::InvalidDefinition("Skill 结构非法或含未知字段"))?;
        validate_skill(&skill, &path)?;
        if !seen.insert(skill.id.clone()) {
            return Err(SkillLoadError::DuplicateId("Skill id 重复"));
        }
        skills.push(skill);
    }
    skills.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(skills)
}

fn validate_skill(skill: &BundledSkill, path: &Path) -> Result<(), SkillLoadError> {
    if skill.id.trim().is_empty() || skill.id.chars().count() > 128 {
        return Err(SkillLoadError::InvalidDefinition("Skill id 为空或超长"));
    }
    if skill.guidance.trim().is_empty() {
        return Err(SkillLoadError::InvalidDefinition("Skill 缺少 guidance"));
    }
    let mut version_parts = skill.version.split('.');
    let valid = version_parts.clone().count() == 3
        && version_parts.all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        });
    if !valid {
        let _ = path;
        return Err(SkillLoadError::InvalidVersion(
            "Skill 版本必须是 x.y.z 数字三段",
        ));
    }
    Ok(())
}

/// 运行指纹输入：按 id 排序后的 `id@version` 列表，经 SHA-256 归一。
///
/// 与既有精彩运行的 `skills_snapshot`（`id@version`）语义一致，因此文件化 Skill 与
/// 常量化 Skill 可以互相对账。
pub fn skills_fingerprint(skills: &[BundledSkill]) -> String {
    let mut ids: Vec<String> = skills
        .iter()
        .map(|skill| format!("{}@{}", skill.id, skill.version))
        .collect();
    ids.sort();
    let joined = ids.join(",");
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(joined.as_bytes()))
}

/// 由一组随包 Skill 恢复常量式 `HighlightSkill` 的等价信息（供既有链路引用）。
pub fn skill_snapshot(skills: &[BundledSkill]) -> Vec<String> {
    let mut snapshot: Vec<String> = skills
        .iter()
        .map(|skill| format!("{}@{}", skill.id, skill.version))
        .collect();
    snapshot.sort();
    snapshot
}

/// 定位随包 Skill 目录：`resources/harness-source/skills`（资产未就位时返回 None）。
pub fn bundled_skills_directory(workspace_root: &Path) -> Option<PathBuf> {
    let candidate = workspace_root
        .join("resources")
        .join("harness-source")
        .join("skills");
    candidate.is_dir().then_some(candidate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_skill(directory: &Path, name: &str, content: &str) {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join(format!("{name}.skill.json")), content).unwrap();
    }

    #[test]
    fn loads_valid_skills_and_produces_a_stable_fingerprint() {
        let directory = tempfile::tempdir().unwrap();
        write_skill(
            directory.path(),
            "generic-hook",
            r#"{"id":"generic-hook","version":"1.0.0","labels":[],"guidance":"开场抓人"}"#,
        );
        write_skill(
            directory.path(),
            "ecommerce-conversion",
            r#"{"id":"ecommerce-conversion","version":"1.0.0","labels":["带货"],"guidance":"卖点与转化"}"#,
        );
        let skills = load_bundled_skills(directory.path()).unwrap();
        assert_eq!(skills.len(), 2);
        let first = skills_fingerprint(&skills);
        let second = skills_fingerprint(&skills);
        assert_eq!(first, second, "指纹必须确定");
        assert_eq!(first.len(), 64, "SHA-256 十六进制");
        assert_eq!(
            skill_snapshot(&skills),
            vec![
                "ecommerce-conversion@1.0.0".to_owned(),
                "generic-hook@1.0.0".to_owned()
            ]
        );
        // 版本变化 → 指纹变化。
        let mut changed = skills;
        changed[0].version = "1.0.1".to_owned();
        assert_ne!(skills_fingerprint(&changed), first);
    }

    #[test]
    fn rejects_unknown_fields_duplicates_and_invalid_versions() {
        let directory = tempfile::tempdir().unwrap();
        // 未知字段：deny_unknown_fields 拒绝（防止随包资产夹带未声明的工具/权限）。
        write_skill(
            directory.path(),
            "sneaky",
            r#"{"id":"sneaky","version":"1.0.0","guidance":"x","tools":["shell"]}"#,
        );
        assert_eq!(
            load_bundled_skills(directory.path()).unwrap_err().code(),
            "skill_definition_invalid"
        );
        // 非法版本。
        let clean = tempfile::tempdir().unwrap();
        write_skill(
            clean.path(),
            "bad-version",
            r#"{"id":"bad-version","version":"latest","guidance":"x"}"#,
        );
        assert_eq!(
            load_bundled_skills(clean.path()).unwrap_err().code(),
            "skill_version_invalid"
        );
        // 重复 id。
        let dup = tempfile::tempdir().unwrap();
        write_skill(
            dup.path(),
            "a",
            r#"{"id":"same","version":"1.0.0","guidance":"x"}"#,
        );
        write_skill(
            dup.path(),
            "b",
            r#"{"id":"same","version":"1.0.0","guidance":"y"}"#,
        );
        assert_eq!(
            load_bundled_skills(dup.path()).unwrap_err().code(),
            "skill_duplicate_id"
        );
    }

    #[test]
    fn missing_directory_is_a_distinguishable_error() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("nope");
        assert_eq!(
            load_bundled_skills(&missing).unwrap_err().code(),
            "skill_directory_missing"
        );
    }
}
