//! 带版本、确定性且不修改原始文本的中文转写后处理。

/// 带版本的中文转写规范化器。
///
/// 第一版只执行确定性的空白、常见中英文标点和金额格式规范化。简繁转换由随包 OpenCC
/// 字符映射文件驱动；映射缺失时保持原文，绝不猜测或修改 `raw_text`。
#[derive(Debug, Clone)]
pub struct TextNormalizer {
    version: String,
    traditional_to_simplified: std::collections::HashMap<char, char>,
}

impl TextNormalizer {
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            traditional_to_simplified: std::collections::HashMap::new(),
        }
    }

    pub fn with_opencc_characters(version: impl Into<String>, mapping: &str) -> Self {
        let mut characters = std::collections::HashMap::new();
        for line in mapping.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let mut fields = line.split_whitespace();
            let Some(traditional) = fields.next().and_then(|value| value.chars().next()) else {
                continue;
            };
            let Some(simplified) = fields.next().and_then(|value| value.chars().next()) else {
                continue;
            };
            characters.insert(traditional, simplified);
        }
        Self {
            version: version.into(),
            traditional_to_simplified: characters,
        }
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn normalize(&self, raw: &str) -> String {
        let simplified: String = raw
            .trim()
            .chars()
            .map(|character| {
                self.traditional_to_simplified
                    .get(&character)
                    .copied()
                    .unwrap_or(character)
            })
            .collect();
        let mut normalized = String::with_capacity(simplified.len());
        let mut previous_whitespace = false;
        for character in simplified.chars() {
            if character.is_whitespace() {
                if !previous_whitespace && !normalized.is_empty() {
                    normalized.push(' ');
                }
                previous_whitespace = true;
                continue;
            }
            previous_whitespace = false;
            normalized.push(match character {
                ',' => '，',
                '!' => '！',
                '?' => '？',
                ':' => '：',
                ';' => '；',
                _ => character,
            });
        }
        let normalized = normalized.trim().to_owned();
        normalize_money(&normalized)
    }
}

fn normalize_money(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < characters.len() {
        if characters[index] == '￥' || characters[index] == '¥' {
            index += 1;
            let start = index;
            while index < characters.len()
                && (characters[index].is_ascii_digit() || characters[index] == '.')
            {
                index += 1;
            }
            if start < index {
                for character in &characters[start..index] {
                    output.push(*character);
                }
                output.push('元');
                continue;
            }
            output.push('￥');
            continue;
        }
        output.push(characters[index]);
        index += 1;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::TextNormalizer;

    #[test]
    fn normalizes_simplified_punctuation_whitespace_and_money_without_changing_raw_input() {
        let mapping = "歡 欢\n間 间\n價 价\n";
        let normalizer = TextNormalizer::with_opencc_characters("zh-normalize-v1", mapping);
        let raw = " 歡迎  來到直播間, 價格是￥99! ";
        assert_eq!(normalizer.normalize(raw), "欢迎 來到直播间， 价格是99元！");
        assert_eq!(raw, " 歡迎  來到直播間, 價格是￥99! ");
        assert_eq!(normalizer.version(), "zh-normalize-v1");
    }
}
