//! 特效字幕组合的白名单校验（步骤 3 任务组 3）。
//!
//! 信任边界：模型只允许产出纯声明式 HTML/CSS，**页面中的可执行内容只能来自随包渲染器的
//! 钉版本引擎脚本**。因此本模块不是"尽力过滤"，而是**白名单 + 失败即拒**：
//!
//! 1. 只接受本文件枚举的标签、属性、CSS 属性与取值函数；其余一律拒绝（见 `TAG_ALLOWLIST` 等）；
//! 2. 任何解析不确定的输入（未闭合标签、嵌套块、未识别的语法）都按拒绝处理，不倒推"可能是无害的"；
//! 3. 拒绝路径**不回显组合正文**：`detail` 只承载结构性标识（标签名、属性名、CSS 属性名、序号），
//!    不含文本内容、URL 或路径；
//! 4. 只有 `ValidatedEffectCaption` 这个类型能进入渲染与落库，调用方无法"跳过校验"。
//!
//! 依赖约束：不引入 HTML/CSS 解析器（`src-tauri` 无此依赖且构建要求 `--offline`），
//! 因此这里实现的是**面向受限子集的严格扫描器**，接受面比通用解析器窄得多。

use serde::Serialize;
use sha2::{Digest, Sha256};

/// 允许出现的标签。`meta` 只允许 `charset`，`style` 的正文按 CSS 规则单独校验。
pub const TAG_ALLOWLIST: &[&str] = &[
    "html", "head", "meta", "title", "style", "body", "div", "span", "p", "br", "strong", "em",
    "b", "i", "small", "u", "s", "ul", "ol", "li",
];

/// 允许出现的全局属性。渲染器契约属性单独列出，且取值受约束。
pub const GLOBAL_ATTRIBUTE_ALLOWLIST: &[&str] = &["class", "style", "lang", "dir", "title"];

/// 渲染器在根元素上要求的契约属性（取值另行校验）。
/// HTML 空元素：不参与嵌套深度与闭合匹配。
pub const VOID_ELEMENTS: &[&str] = &["meta", "br"];

pub const RENDERER_CONTRACT_ATTRIBUTES: &[&str] = &[
    "data-composition-id",
    "data-duration",
    "data-width",
    "data-height",
    "data-no-timeline",
];

/// 允许出现的 CSS 属性。含 `-webkit-` 前缀的常见别名需显式列出，否则按未知属性拒绝。
pub const CSS_PROPERTY_ALLOWLIST: &[&str] = &[
    "align-content",
    "align-items",
    "align-self",
    "animation",
    "animation-delay",
    "animation-direction",
    "animation-duration",
    "animation-fill-mode",
    "animation-iteration-count",
    "animation-name",
    "animation-play-state",
    "animation-timing-function",
    "background",
    "background-clip",
    "background-color",
    "background-image",
    "background-position",
    "background-repeat",
    "background-size",
    "border",
    "border-bottom",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-bottom-width",
    "border-color",
    "border-left",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-radius",
    "border-right",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-style",
    "border-top",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-top-width",
    "border-width",
    "bottom",
    "box-shadow",
    "box-sizing",
    "clip-path",
    "color",
    "column-gap",
    "content",
    "display",
    "filter",
    "flex",
    "flex-basis",
    "flex-direction",
    "flex-grow",
    "flex-shrink",
    "flex-wrap",
    "font-family",
    "font-size",
    "font-style",
    "font-variant-numeric",
    "font-weight",
    "gap",
    "grid-auto-flow",
    "grid-column",
    "grid-row",
    "grid-template-columns",
    "grid-template-rows",
    "height",
    "inset",
    "justify-content",
    "justify-items",
    "justify-self",
    "left",
    "letter-spacing",
    "line-height",
    "margin",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "mix-blend-mode",
    "object-position",
    "opacity",
    "order",
    "outline",
    "outline-color",
    "outline-offset",
    "outline-style",
    "outline-width",
    "overflow",
    "overflow-wrap",
    "padding",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "position",
    "right",
    "row-gap",
    "text-align",
    "text-decoration",
    "text-decoration-color",
    "text-decoration-line",
    "text-decoration-style",
    "text-shadow",
    "text-transform",
    "top",
    "transform",
    "transform-origin",
    "transition",
    "transition-delay",
    "transition-duration",
    "transition-property",
    "transition-timing-function",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "will-change",
    "word-break",
    "word-spacing",
    "writing-mode",
    "z-index",
    "-webkit-text-stroke",
    "-webkit-text-stroke-color",
    "-webkit-text-stroke-width",
];

/// 允许出现的 CSS 函数。未列出的函数一律拒绝（fail-closed）。
pub const CSS_FUNCTION_ALLOWLIST: &[&str] = &[
    "blur",
    "brightness",
    "calc",
    "clamp",
    "contrast",
    "cubic-bezier",
    "drop-shadow",
    "grayscale",
    "hsl",
    "hsla",
    "hue-rotate",
    "invert",
    "linear-gradient",
    "max",
    "min",
    "radial-gradient",
    "repeat",
    "rgb",
    "rgba",
    "saturate",
    "sepia",
    "steps",
    "translate",
    "translate3d",
    "translateX",
    "translateY",
    "translateZ",
    "rotate",
    "rotateX",
    "rotateY",
    "rotateZ",
    "scale",
    "scale3d",
    "scaleX",
    "scaleY",
    "skew",
    "skewX",
    "skewY",
    "var",
];

/// 值里出现即拒绝的子串：外部引用、可执行内容、非确定性与构建配置。
pub const FORBIDDEN_VALUE_SUBSTRINGS: &[&str] = &[
    "url(",
    "javascript:",
    "data:",
    "vbscript:",
    "expression(",
    "@import",
    "import(",
    "image-set(",
    "-webkit-image-set(",
    "random(",
    "random-item(",
    "env(",
    "attr(",
    "scroll(",
    "view(",
    "counter(",
    "target-counter(",
    "element(",
    "paint(",
    "<<<",
    ">>>",
    "{{",
    "}}",
    "/*",
    "*/",
];

/// 允许的注释形式之外，正文里出现即拒绝的构建配置信号。
pub const FORBIDDEN_DOCUMENT_SUBSTRINGS: &[&str] = &[
    "<!entity",
    "<?xml",
    "<script",
    "onload=",
    "onerror=",
    "<link",
    "<base",
    "<iframe",
    "<object",
    "<embed",
    "<form",
    "<input",
    "<button",
    "<template",
    "<noscript",
    "<svg",
    "<math",
    "<img",
    "<video",
    "<audio",
    "<source",
    "<track",
    "<canvas",
    "package.json",
    "node_modules",
    "npm install",
    "yarn add",
    "pnpm add",
    "<build",
    "webpack",
];

/// 组合冻结上限。数值来自设计决策三的口径："受控上限且超限即拒绝"，具体数值在实现期确定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectCaptionLimits {
    pub max_elements: usize,
    pub max_nesting_depth: usize,
    pub max_animation_ms: u64,
    pub max_composition_ms: u64,
    pub max_frames: u64,
    pub max_css_bytes: usize,
    pub frames_per_second: u32,
    pub canvas_width: u32,
    pub canvas_height: u32,
}

impl Default for EffectCaptionLimits {
    fn default() -> Self {
        Self {
            max_elements: 512,
            max_nesting_depth: 24,
            max_animation_ms: 60_000,
            max_composition_ms: 180_000,
            max_frames: 5_400,
            max_css_bytes: 64 * 1024,
            // 决策四：30 fps 为上限，60 fps 明确不支持。
            frames_per_second: 30,
            canvas_width: 1080,
            canvas_height: 1920,
        }
    }
}

/// 拒绝原因。`detail` 只承载结构性标识，**不含**组合正文、URL 或本地路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct EffectCaptionRejection {
    pub code: &'static str,
    pub detail: String,
}

impl EffectCaptionRejection {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

/// 通过校验的组合。只有这个类型可以进入渲染与落库（任务 3.9 的类型级保证）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedEffectCaption {
    /// 组合字节的 sha256，作为组合版本进入渲染缓存键。
    pub fingerprint: String,
    pub composition_id: String,
    pub composition_ms: u64,
    pub element_count: usize,
    pub max_depth: usize,
    pub longest_animation_ms: u64,
    pub frames: u64,
    pub text_runs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScannedElement {
    name: String,
    attributes: Vec<(String, String)>,
    self_closing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HtmlToken {
    Element(ScannedElement),
    CloseElement(String),
    Text(String),
    Style(String),
    Comment(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CssDeclaration {
    property: String,
    value: String,
}

/// 校验一个特效字幕组合。
///
/// `allowed_texts` 是该高光已发布的工程字幕、用户修正文案与已冻结强调词的集合；
/// 组合中出现的每一段文字都必须是其中之一，否则拒绝。
pub fn validate_effect_caption(
    html: &str,
    allowed_texts: &[String],
    limits: &EffectCaptionLimits,
) -> Result<ValidatedEffectCaption, EffectCaptionRejection> {
    let fingerprint = hex::encode(Sha256::digest(html.as_bytes()));
    let tokens = scan_html(html)?;
    let mut stack: Vec<String> = Vec::new();
    let mut element_count = 0_usize;
    let mut max_depth = 0_usize;
    let mut text_runs = 0_usize;
    let mut longest_animation_ms = 0_u64;
    let mut css_bytes = 0_usize;
    let mut root_attributes: Vec<(String, String)> = Vec::new();
    let mut root_seen = false;
    let mut opaque_root_background: Option<String> = None;

    for token in &tokens {
        match token {
            HtmlToken::Comment(body) => {
                // 注释同样不得承载脚本或构建配置，避免"注释里藏指令"绕过。
                let lowered = body.to_lowercase();
                if let Some(hit) = first_forbidden(&lowered, FORBIDDEN_DOCUMENT_SUBSTRINGS) {
                    return Err(EffectCaptionRejection::new(
                        "effect_script_rejected",
                        format!("注释包含被禁止的内容信号 {hit}"),
                    ));
                }
            }
            HtmlToken::Text(body) => {
                let trimmed = body.trim();
                if trimmed.is_empty() {
                    continue;
                }
                // <title> 只用于渲染器诊断，不绘制到画布上，因此不受字幕语料约束。
                if stack.last().map(String::as_str) == Some("title") {
                    continue;
                }
                text_runs += 1;
                if !allowed_texts
                    .iter()
                    .any(|allowed| !allowed.is_empty() && allowed.contains(trimmed))
                {
                    return Err(EffectCaptionRejection::new(
                        "effect_text_reference_rejected",
                        format!("第 {text_runs} 段文字不在已发布的字幕、修正文案或冻结强调词内"),
                    ));
                }
            }
            HtmlToken::Style(raw) => {
                css_bytes = css_bytes.saturating_add(raw.len());
                if css_bytes > limits.max_css_bytes {
                    return Err(EffectCaptionRejection::new(
                        "effect_limits_exceeded",
                        format!("样式总字节 {} 超过上限 {}", css_bytes, limits.max_css_bytes),
                    ));
                }
                let longest = validate_css(raw, allowed_texts, limits)?;
                longest_animation_ms = longest_animation_ms.max(longest);
            }
            HtmlToken::Element(element) => {
                element_count += 1;
                if element_count > limits.max_elements {
                    return Err(EffectCaptionRejection::new(
                        "effect_limits_exceeded",
                        format!("元素数量超过上限 {}", limits.max_elements),
                    ));
                }
                if !TAG_ALLOWLIST.contains(&element.name.as_str()) {
                    return Err(EffectCaptionRejection::new(
                        "effect_unsupported_tag",
                        format!("标签 {} 不在白名单内", element.name),
                    ));
                }
                validate_element_attributes(element)?;
                if !root_seen {
                    root_seen = true;
                    root_attributes = element.attributes.clone();
                }
                if element.name == "meta" {
                    let charset = element
                        .attributes
                        .iter()
                        .find(|(name, _)| name == "charset")
                        .map(|(_, value)| value.as_str());
                    if charset.is_none() {
                        return Err(EffectCaptionRejection::new(
                            "effect_unsupported_attribute",
                            "meta 只允许 charset 属性",
                        ));
                    }
                }
                if (element.name == "html" || element.name == "body")
                    && let Some(hit) = opaque_background_in_inline_style(element)
                {
                    opaque_root_background.get_or_insert(hit);
                }
                if !element.self_closing && !VOID_ELEMENTS.contains(&element.name.as_str()) {
                    stack.push(element.name.clone());
                    max_depth = max_depth.max(stack.len());
                    if stack.len() > limits.max_nesting_depth {
                        return Err(EffectCaptionRejection::new(
                            "effect_limits_exceeded",
                            format!("嵌套深度超过上限 {}", limits.max_nesting_depth),
                        ));
                    }
                }
            }
            HtmlToken::CloseElement(name) => {
                let Some(open) = stack.pop() else {
                    return Err(EffectCaptionRejection::new(
                        "effect_caption_malformed",
                        format!("出现了没有对应开始标签的 </{name}>"),
                    ));
                };
                if open != *name {
                    return Err(EffectCaptionRejection::new(
                        "effect_caption_malformed",
                        format!("标签未正确闭合：<{open}> 与 </{name}>"),
                    ));
                }
            }
        }
    }
    if !stack.is_empty() {
        return Err(EffectCaptionRejection::new(
            "effect_caption_malformed",
            format!("有 {} 个标签未闭合", stack.len()),
        ));
    }
    if !root_seen {
        return Err(EffectCaptionRejection::new(
            "effect_caption_malformed",
            "组合里没有任何元素",
        ));
    }
    if let Some(property) = opaque_root_background {
        return Err(EffectCaptionRejection::new(
            "effect_opaque_background_rejected",
            format!("根元素或 body 通过内联 {property} 绘制了不透明背景"),
        ));
    }

    // 渲染器契约：根元素必须声明画布、时长与组合标识，否则渲染无法确定规格。
    let composition_id = contract_attribute(&root_attributes, "data-composition-id")?;
    let duration_seconds = contract_attribute(&root_attributes, "data-duration")?
        .parse::<u64>()
        .map_err(|_| {
            EffectCaptionRejection::new("effect_caption_malformed", "data-duration 必须是整数秒")
        })?;
    if duration_seconds == 0 {
        return Err(EffectCaptionRejection::new(
            "effect_caption_malformed",
            "data-duration 必须大于 0",
        ));
    }
    let composition_ms = duration_seconds.saturating_mul(1_000);
    if composition_ms > limits.max_composition_ms {
        return Err(EffectCaptionRejection::new(
            "effect_limits_exceeded",
            format!(
                "组合时长 {composition_ms}ms 超过上限 {}ms",
                limits.max_composition_ms
            ),
        ));
    }
    let canvas_width = contract_attribute(&root_attributes, "data-width")?
        .parse::<u32>()
        .map_err(|_| {
            EffectCaptionRejection::new("effect_caption_malformed", "data-width 必须是整数")
        })?;
    let canvas_height = contract_attribute(&root_attributes, "data-height")?
        .parse::<u32>()
        .map_err(|_| {
            EffectCaptionRejection::new("effect_caption_malformed", "data-height 必须是整数")
        })?;
    if canvas_width != limits.canvas_width || canvas_height != limits.canvas_height {
        return Err(EffectCaptionRejection::new(
            "effect_caption_malformed",
            format!(
                "画布 {canvas_width}x{canvas_height} 与冻结规格 {}x{} 不一致",
                limits.canvas_width, limits.canvas_height
            ),
        ));
    }
    if longest_animation_ms > limits.max_animation_ms {
        return Err(EffectCaptionRejection::new(
            "effect_limits_exceeded",
            format!(
                "动画时长 {longest_animation_ms}ms 超过上限 {}ms",
                limits.max_animation_ms
            ),
        ));
    }
    let frames = composition_ms.saturating_mul(u64::from(limits.frames_per_second)) / 1_000;
    if frames > limits.max_frames {
        return Err(EffectCaptionRejection::new(
            "effect_limits_exceeded",
            format!("帧数 {frames} 超过上限 {}", limits.max_frames),
        ));
    }

    Ok(ValidatedEffectCaption {
        fingerprint,
        composition_id,
        composition_ms,
        element_count,
        max_depth,
        longest_animation_ms,
        frames,
        text_runs,
    })
}

fn contract_attribute(
    attributes: &[(String, String)],
    name: &'static str,
) -> Result<String, EffectCaptionRejection> {
    attributes
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
        .ok_or_else(|| {
            EffectCaptionRejection::new(
                "effect_caption_malformed",
                format!("根元素缺少渲染器契约属性 {name}"),
            )
        })
}

fn first_forbidden<'a>(haystack: &str, needles: &[&'a str]) -> Option<&'a str> {
    needles
        .iter()
        .copied()
        .find(|needle| haystack.contains(needle))
}

fn validate_element_attributes(element: &ScannedElement) -> Result<(), EffectCaptionRejection> {
    for (name, value) in &element.attributes {
        let lowered_name = name.to_lowercase();
        if lowered_name.starts_with("on") {
            return Err(EffectCaptionRejection::new(
                "effect_event_handler_rejected",
                format!("<{}> 上出现事件处理器属性 {name}", element.name),
            ));
        }
        if lowered_name == "src"
            || lowered_name == "href"
            || lowered_name == "srcset"
            || lowered_name == "action"
            || lowered_name == "formaction"
            || lowered_name == "poster"
            || lowered_name == "background"
            || lowered_name == "integrity"
            || lowered_name == "nonce"
            || lowered_name == "http-equiv"
            || lowered_name == "xlink:href"
            || lowered_name == "xmlns"
        {
            return Err(EffectCaptionRejection::new(
                "effect_external_reference_rejected",
                format!("<{}> 上出现引用型属性 {name}", element.name),
            ));
        }
        let is_contract = RENDERER_CONTRACT_ATTRIBUTES.contains(&lowered_name.as_str());
        let is_meta_charset = element.name == "meta" && lowered_name == "charset";
        if !is_contract
            && !is_meta_charset
            && !GLOBAL_ATTRIBUTE_ALLOWLIST.contains(&lowered_name.as_str())
        {
            return Err(EffectCaptionRejection::new(
                "effect_unsupported_attribute",
                format!("<{}> 上出现白名单外属性 {name}", element.name),
            ));
        }
        let lowered_value = value.to_lowercase();
        if let Some(hit) = first_forbidden(&lowered_value, FORBIDDEN_VALUE_SUBSTRINGS) {
            return Err(EffectCaptionRejection::new(
                "effect_external_reference_rejected",
                format!("属性 {name} 的取值包含被禁止的内容 {hit}"),
            ));
        }
        if lowered_name == "style" {
            validate_inline_css(value, element)?;
        }
    }
    if element.name == "meta" {
        let keys: Vec<&str> = element
            .attributes
            .iter()
            .map(|(name, _)| name.as_str())
            .collect();
        if keys != ["charset"] {
            return Err(EffectCaptionRejection::new(
                "effect_unsupported_attribute",
                "meta 只允许 charset 属性",
            ));
        }
    }
    Ok(())
}

/// 内联 `style` 与样式表走同一套声明校验，避免"样式表管住了、内联漏了"。
fn validate_inline_css(
    value: &str,
    element: &ScannedElement,
) -> Result<(), EffectCaptionRejection> {
    for declaration in parse_declarations(value)? {
        validate_declaration(&declaration, &format!("<{}> 的内联样式", element.name))?;
    }
    Ok(())
}

fn opaque_background_in_inline_style(element: &ScannedElement) -> Option<String> {
    let style = element
        .attributes
        .iter()
        .find(|(name, _)| name == "style")
        .map(|(_, value)| value.as_str())?;
    let declarations = parse_declarations(style).ok()?;
    declarations
        .into_iter()
        .find(|declaration| {
            matches!(
                declaration.property.as_str(),
                "background" | "background-color"
            ) && !is_transparent_background(&declaration.value)
        })
        .map(|declaration| declaration.property)
}

fn scan_html(html: &str) -> Result<Vec<HtmlToken>, EffectCaptionRejection> {
    let lowered = html.to_lowercase();
    if let Some(hit) = first_forbidden(&lowered, FORBIDDEN_DOCUMENT_SUBSTRINGS) {
        return Err(EffectCaptionRejection::new(
            "effect_script_rejected",
            format!("组合包含被禁止的内容信号 {hit}"),
        ));
    }

    let bytes = html.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'<' {
            let start = index;
            while index < bytes.len() && bytes[index] != b'<' {
                index += 1;
            }
            tokens.push(HtmlToken::Text(html[start..index].to_owned()));
            continue;
        }
        if lowered[index..].starts_with("<!--") {
            let end = lowered[index..]
                .find("-->")
                .map(|offset| index + offset + 3)
                .ok_or_else(|| {
                    EffectCaptionRejection::new("effect_caption_malformed", "注释没有闭合")
                })?;
            tokens.push(HtmlToken::Comment(html[index + 4..end - 3].to_owned()));
            index = end;
            continue;
        }
        if lowered[index..].starts_with("<!doctype") {
            let end = lowered[index..]
                .find('>')
                .map(|offset| index + offset + 1)
                .ok_or_else(|| {
                    EffectCaptionRejection::new("effect_caption_malformed", "doctype 没有闭合")
                })?;
            index = end;
            continue;
        }
        if lowered[index..].starts_with("</") {
            let end = lowered[index..]
                .find('>')
                .map(|offset| index + offset)
                .ok_or_else(|| {
                    EffectCaptionRejection::new("effect_caption_malformed", "结束标签没有闭合")
                })?;
            let name = lowered[index + 2..end].trim().to_owned();
            if name.is_empty() || !name.chars().all(is_name_char) {
                return Err(EffectCaptionRejection::new(
                    "effect_caption_malformed",
                    "结束标签名非法",
                ));
            }
            tokens.push(HtmlToken::CloseElement(name));
            index = end + 1;
            continue;
        }
        let end = find_tag_end(html, index)?;
        let inner = &html[index + 1..end];
        let self_closing = inner.trim_end().ends_with('/');
        let body = if self_closing {
            inner.trim_end().trim_end_matches('/')
        } else {
            inner
        };
        let element = parse_element(body)?;
        let is_style = element.name == "style";
        tokens.push(HtmlToken::Element(element));
        index = end + 1;
        if is_style && !self_closing {
            let lowered_tail = &lowered[index..];
            let close = lowered_tail.find("</style").ok_or_else(|| {
                EffectCaptionRejection::new("effect_caption_malformed", "style 没有闭合")
            })?;
            let raw = &html[index..index + close];
            tokens.push(HtmlToken::Style(raw.to_owned()));
            let close_end = lowered[index + close..]
                .find('>')
                .map(|offset| index + close + offset + 1)
                .ok_or_else(|| {
                    EffectCaptionRejection::new("effect_caption_malformed", "style 结束标签非法")
                })?;
            tokens.push(HtmlToken::CloseElement("style".to_owned()));
            index = close_end;
        }
    }
    Ok(tokens)
}

fn find_tag_end(html: &str, start: usize) -> Result<usize, EffectCaptionRejection> {
    let bytes = html.as_bytes();
    let mut quote: Option<u8> = None;
    let mut index = start + 1;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(active) => {
                if byte == active {
                    quote = None;
                }
            }
            None => {
                if byte == b'"' || byte == b'\'' {
                    quote = Some(byte);
                } else if byte == b'>' {
                    return Ok(index);
                } else if byte == b'<' {
                    return Err(EffectCaptionRejection::new(
                        "effect_caption_malformed",
                        "标签里出现未转义的 '<'",
                    ));
                }
            }
        }
        index += 1;
    }
    Err(EffectCaptionRejection::new(
        "effect_caption_malformed",
        "标签没有闭合",
    ))
}

fn parse_element(body: &str) -> Result<ScannedElement, EffectCaptionRejection> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(EffectCaptionRejection::new(
            "effect_caption_malformed",
            "出现空的标签",
        ));
    }
    let mut chars = trimmed.char_indices();
    let name_end = chars
        .find(|(_, character)| character.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let name = trimmed[..name_end].to_lowercase();
    if name.is_empty() || !name.chars().all(is_name_char) {
        return Err(EffectCaptionRejection::new(
            "effect_caption_malformed",
            "标签名非法",
        ));
    }
    let attributes = parse_attributes(&trimmed[name_end..])?;
    Ok(ScannedElement {
        name,
        attributes,
        self_closing: trimmed.ends_with('/'),
    })
}

fn is_name_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':')
}

fn parse_attributes(input: &str) -> Result<Vec<(String, String)>, EffectCaptionRejection> {
    let mut attributes = Vec::new();
    let bytes = input.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] as char).is_whitespace() {
            index += 1;
        }
        if index >= bytes.len() || bytes[index] == b'/' {
            break;
        }
        let name_start = index;
        while index < bytes.len()
            && !(bytes[index] as char).is_whitespace()
            && bytes[index] != b'='
            && bytes[index] != b'/'
        {
            index += 1;
        }
        let name = input[name_start..index].to_lowercase();
        if name.is_empty() || !name.chars().all(is_name_char) {
            return Err(EffectCaptionRejection::new(
                "effect_caption_malformed",
                "属性名非法",
            ));
        }
        while index < bytes.len() && (bytes[index] as char).is_whitespace() {
            index += 1;
        }
        let mut value = String::new();
        if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            while index < bytes.len() && (bytes[index] as char).is_whitespace() {
                index += 1;
            }
            if index >= bytes.len() {
                return Err(EffectCaptionRejection::new(
                    "effect_caption_malformed",
                    format!("属性 {name} 缺少取值"),
                ));
            }
            let quote = bytes[index];
            if quote == b'"' || quote == b'\'' {
                index += 1;
                let value_start = index;
                while index < bytes.len() && bytes[index] != quote {
                    index += 1;
                }
                if index >= bytes.len() {
                    return Err(EffectCaptionRejection::new(
                        "effect_caption_malformed",
                        format!("属性 {name} 的引号没有闭合"),
                    ));
                }
                value = input[value_start..index].to_owned();
                index += 1;
            } else {
                let value_start = index;
                while index < bytes.len() && !(bytes[index] as char).is_whitespace() {
                    index += 1;
                }
                value = input[value_start..index].to_owned();
            }
        }
        attributes.push((name, value));
    }
    Ok(attributes)
}

fn parse_declarations(input: &str) -> Result<Vec<CssDeclaration>, EffectCaptionRejection> {
    let mut declarations = Vec::new();
    for chunk in split_top_level(input, ';') {
        let trimmed = chunk.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Some((property, value)) = trimmed.split_once(':') else {
            return Err(EffectCaptionRejection::new(
                "effect_caption_malformed",
                "CSS 声明缺少冒号",
            ));
        };
        let property = property.trim().to_lowercase();
        if property.is_empty() || !property.chars().all(is_css_ident_char) {
            return Err(EffectCaptionRejection::new(
                "effect_caption_malformed",
                "CSS 属性名非法",
            ));
        }
        declarations.push(CssDeclaration {
            property,
            value: value.trim().to_owned(),
        });
    }
    Ok(declarations)
}

fn is_css_ident_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

fn split_top_level(input: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut start = 0_usize;
    for (index, character) in input.char_indices() {
        match quote {
            Some(active) => {
                if character == active {
                    quote = None;
                }
            }
            None => match character {
                '"' | '\'' => quote = Some(character),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                _ if character == separator && depth == 0 => {
                    parts.push(&input[start..index]);
                    start = index + character.len_utf8();
                }
                _ => {}
            },
        }
    }
    parts.push(&input[start..]);
    parts
}

/// 校验整段样式表，返回其中出现过的最长动画时长。
fn validate_css(
    css: &str,
    allowed_texts: &[String],
    limits: &EffectCaptionLimits,
) -> Result<u64, EffectCaptionRejection> {
    let lowered = css.to_lowercase();
    if let Some(hit) = first_forbidden(&lowered, FORBIDDEN_VALUE_SUBSTRINGS) {
        let code = if hit.contains("url(") || hit.ends_with("image-set(") || hit == "@import" {
            "effect_external_reference_rejected"
        } else if matches!(
            hit,
            "random(" | "random-item(" | "env(" | "scroll(" | "view("
        ) {
            "effect_nondeterministic_rejected"
        } else if hit == "/*" || hit == "*/" {
            "effect_unsupported_css_value"
        } else {
            "effect_script_rejected"
        };
        return Err(EffectCaptionRejection::new(
            code,
            format!("样式表包含被禁止的内容 {hit}"),
        ));
    }

    let mut longest = 0_u64;
    let mut rest = css;
    while let Some(open) = rest.find('{') {
        let prelude = rest[..open].trim().to_owned();
        let close = matching_brace(rest, open)?;
        let block = &rest[open + 1..close];
        if prelude.starts_with('@') {
            let lowered_prelude = prelude.to_lowercase();
            if !lowered_prelude.starts_with("@keyframes") {
                return Err(EffectCaptionRejection::new(
                    "effect_unsupported_css_property",
                    format!(
                        "只允许 @keyframes，出现 {}",
                        prelude.split_whitespace().next().unwrap_or("@")
                    ),
                ));
            }
            // @keyframes 内部按"选择器 { 声明 }"继续校验。
            let mut inner = block;
            while let Some(inner_open) = inner.find('{') {
                let inner_close = matching_brace(inner, inner_open)?;
                let inner_declarations = &inner[inner_open + 1..inner_close];
                longest = longest.max(validate_declaration_block(
                    inner_declarations,
                    allowed_texts,
                    limits,
                )?);
                inner = &inner[inner_close + 1..];
            }
            rest = &rest[close + 1..];
            continue;
        }
        if prelude.is_empty() {
            return Err(EffectCaptionRejection::new(
                "effect_caption_malformed",
                "CSS 规则缺少选择器",
            ));
        }
        if block.contains('{') {
            return Err(EffectCaptionRejection::new(
                "effect_unsupported_css_property",
                "不支持 CSS 嵌套规则",
            ));
        }
        let rule_longest = validate_declaration_block(block, allowed_texts, limits)?;
        longest = longest.max(rule_longest);
        if selector_targets_root(&prelude) {
            for declaration in parse_declarations(block)? {
                if matches!(
                    declaration.property.as_str(),
                    "background" | "background-color"
                ) && !is_transparent_background(&declaration.value)
                {
                    return Err(EffectCaptionRejection::new(
                        "effect_opaque_background_rejected",
                        format!("根选择器 {} 绘制了不透明背景", prelude),
                    ));
                }
                if declaration.property == "background-image" {
                    return Err(EffectCaptionRejection::new(
                        "effect_opaque_background_rejected",
                        "根选择器不允许 background-image，透明中间产物会失效",
                    ));
                }
            }
        }
        rest = &rest[close + 1..];
    }
    if rest.contains('}') {
        return Err(EffectCaptionRejection::new(
            "effect_caption_malformed",
            "CSS 出现多余的 '}'",
        ));
    }
    Ok(longest)
}

fn validate_declaration_block(
    block: &str,
    allowed_texts: &[String],
    limits: &EffectCaptionLimits,
) -> Result<u64, EffectCaptionRejection> {
    let mut longest = 0_u64;
    for declaration in parse_declarations(block)? {
        longest = longest.max(validate_declaration_with_limits(
            &declaration,
            allowed_texts,
            limits,
        )?);
    }
    Ok(longest)
}

fn validate_declaration(
    declaration: &CssDeclaration,
    context: &str,
) -> Result<(), EffectCaptionRejection> {
    validate_declaration_with_limits(declaration, &[], &EffectCaptionLimits::default())
        .map(|_| ())
        .map_err(|rejection| {
            EffectCaptionRejection::new(rejection.code, format!("{context}：{}", rejection.detail))
        })
}

fn validate_declaration_with_limits(
    declaration: &CssDeclaration,
    allowed_texts: &[String],
    limits: &EffectCaptionLimits,
) -> Result<u64, EffectCaptionRejection> {
    let lowered_value = declaration.value.to_lowercase();
    if let Some(hit) = first_forbidden(&lowered_value, FORBIDDEN_VALUE_SUBSTRINGS) {
        let code = if hit == "url(" || hit.ends_with("image-set(") {
            "effect_external_reference_rejected"
        } else if matches!(
            hit,
            "random(" | "random-item(" | "env(" | "scroll(" | "view("
        ) {
            "effect_nondeterministic_rejected"
        } else {
            "effect_unsupported_css_value"
        };
        return Err(EffectCaptionRejection::new(
            code,
            format!("{} 的取值包含被禁止的内容 {hit}", declaration.property),
        ));
    }
    if !CSS_PROPERTY_ALLOWLIST.contains(&declaration.property.as_str()) {
        return Err(EffectCaptionRejection::new(
            "effect_unsupported_css_property",
            format!("CSS 属性 {} 不在白名单内", declaration.property),
        ));
    }
    for function in css_functions(&lowered_value) {
        if !CSS_FUNCTION_ALLOWLIST
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(&function))
        {
            return Err(EffectCaptionRejection::new(
                "effect_unsupported_css_value",
                format!("{} 使用了白名单外的函数 {}", declaration.property, function),
            ));
        }
    }
    if declaration.property == "font-family" {
        for family in declaration.value.split(',') {
            let family = family.trim().trim_matches(['"', '\'']).to_lowercase();
            if family.is_empty() {
                continue;
            }
            if !family.starts_with("var(") && !BUNDLED_FONT_FAMILIES.contains(&family.as_str()) {
                return Err(EffectCaptionRejection::new(
                    "effect_text_reference_rejected",
                    format!("字体 {family} 不是随包锁定字体"),
                ));
            }
        }
    }
    if declaration.property == "content" {
        let text = declaration
            .value
            .trim()
            .trim_start_matches(['"', '\''])
            .trim_end_matches(['"', '\'']);
        if !text.is_empty()
            && !allowed_texts
                .iter()
                .any(|allowed| !allowed.is_empty() && allowed.contains(text))
        {
            return Err(EffectCaptionRejection::new(
                "effect_text_reference_rejected",
                "content 生成的文字不在已发布的字幕、修正文案或冻结强调词内",
            ));
        }
    }
    if matches!(
        declaration.property.as_str(),
        "animation-duration" | "transition-duration"
    ) && let Some(ms) = parse_time_ms(&declaration.value)
    {
        return Ok(ms);
    }
    if declaration.property == "animation" || declaration.property == "transition" {
        let mut longest = 0_u64;
        for part in declaration.value.split_whitespace() {
            if let Some(ms) = parse_time_ms(part) {
                longest = longest.max(ms);
            }
        }
        return Ok(longest);
    }
    let _ = limits;
    Ok(0)
}

/// 随包锁定字体族。系统字体不得作为特效字幕的字形来源（任务 5.1）。
pub const BUNDLED_FONT_FAMILIES: &[&str] =
    &["hyperframes-caption-cjk", "hyperframes-caption-latin"];

fn css_functions(value: &str) -> Vec<String> {
    let mut functions = Vec::new();
    let bytes = value.as_bytes();
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] == b'(' {
            let mut start = index;
            while start > 0 {
                let candidate = bytes[start - 1] as char;
                if candidate.is_ascii_alphanumeric() || candidate == '-' {
                    start -= 1;
                } else {
                    break;
                }
            }
            if start < index {
                functions.push(value[start..index].to_lowercase());
            }
        }
        index += 1;
    }
    functions
}

fn matching_brace(input: &str, open: usize) -> Result<usize, EffectCaptionRejection> {
    let bytes = input.as_bytes();
    let mut depth = 0_i32;
    let mut quote: Option<u8> = None;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(active) => {
                if byte == active {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Ok(index);
                    }
                }
                _ => {}
            },
        }
        index += 1;
    }
    Err(EffectCaptionRejection::new(
        "effect_caption_malformed",
        "CSS 区块没有闭合",
    ))
}

fn selector_targets_root(selector_list: &str) -> bool {
    selector_list.split(',').any(|selector| {
        let selector = selector.trim().to_lowercase();
        selector == "*"
            || selector == ":root"
            || selector == "html"
            || selector == "body"
            || selector.starts_with("html,")
            || selector.starts_with("html ")
            || selector.starts_with("body ")
            || selector.starts_with("html>")
    })
}

fn is_transparent_background(value: &str) -> bool {
    let lowered = value.trim().to_lowercase();
    if lowered.is_empty()
        || lowered == "none"
        || lowered == "transparent"
        || lowered == "initial"
        || lowered == "unset"
    {
        return true;
    }
    if lowered.starts_with("var(") {
        return false;
    }
    // rgba()/hsla() 的 alpha 为 0 视为透明；其余一律按不透明处理（fail-closed）。
    if let Some(inner) = lowered
        .strip_prefix("rgba(")
        .or_else(|| lowered.strip_prefix("hsla("))
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if let Some(alpha) = parts.last() {
            let alpha = alpha.trim_end_matches('%');
            if let Ok(alpha) = alpha.parse::<f32>() {
                return alpha <= f32::EPSILON;
            }
        }
    }
    false
}

fn parse_time_ms(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if let Some(number) = trimmed.strip_suffix("ms") {
        return number.trim().parse::<f64>().ok().map(|value| {
            if value.is_finite() && value >= 0.0 {
                value.round() as u64
            } else {
                0
            }
        });
    }
    if let Some(number) = trimmed.strip_suffix('s') {
        return number.trim().parse::<f64>().ok().map(|value| {
            if value.is_finite() && value >= 0.0 {
                (value * 1_000.0).round() as u64
            } else {
                0
            }
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: EffectCaptionLimits = EffectCaptionLimits {
        max_elements: 512,
        max_nesting_depth: 24,
        max_animation_ms: 60_000,
        max_composition_ms: 180_000,
        max_frames: 5_400,
        max_css_bytes: 64 * 1024,
        frames_per_second: 30,
        canvas_width: 1080,
        canvas_height: 1920,
    };

    fn corpus() -> Vec<String> {
        vec!["这个价格真的绷不住了".to_owned(), "，冲".to_owned()]
    }

    fn root_attributes() -> &'static str {
        r#"data-composition-id="main" data-duration="15" data-width="1080" data-height="1920""#
    }

    fn compose(body: &str, css: &str) -> String {
        format!(
            "<!DOCTYPE html>\n<html {attrs}>\n<head>\n<meta charset=\"utf-8\">\n<title>t</title>\n<style>{css}</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
            attrs = root_attributes()
        )
    }

    /// 最小合规组合：用于确认"接受路径"确实能通过。
    fn compliant() -> String {
        compose(
            r#"<div class="stage"><span class="ch">这个价格真的绷不住了</span><span class="emph">，冲</span></div>"#,
            r#"
            html, body { margin:0; padding:0; background:transparent; }
            .stage { position:absolute; left:0; right:0; bottom:230px; }
            .ch { display:inline-block; animation: ch-in 240ms cubic-bezier(.2,.9,.3,1) both; }
            @keyframes ch-in { 0% { opacity:0; transform:translateY(38px) rotate(6deg); } 100% { opacity:1; transform:translateY(0); } }
            "#,
        )
    }

    #[test]
    fn compliant_composition_is_accepted_and_counted() {
        let validated =
            validate_effect_caption(&compliant(), &corpus(), &LIMITS).expect("应当通过");
        assert_eq!(validated.composition_id, "main");
        assert_eq!(validated.composition_ms, 15_000);
        assert_eq!(validated.frames, 450);
        assert_eq!(validated.longest_animation_ms, 240);
        assert_eq!(validated.text_runs, 2);
        assert_eq!(validated.fingerprint.len(), 64);
        assert!(validated.element_count >= 5);
    }

    #[test]
    fn fingerprint_follows_composition_bytes() {
        let first = validate_effect_caption(&compliant(), &corpus(), &LIMITS).unwrap();
        let mut changed = compliant();
        changed = changed.replace("240ms", "250ms");
        let second = validate_effect_caption(&changed, &corpus(), &LIMITS).unwrap();
        assert_ne!(
            first.fingerprint, second.fingerprint,
            "组合版本必须随字节变化"
        );
    }

    // 任务 3.8：九类越界样本，逐一断言被拒且给出稳定错误码。
    #[test]
    fn rejects_script_and_event_handler() {
        let script = compose(
            r#"<div class="x"><script>alert(1)</script></div>"#,
            ".x{opacity:1}",
        );
        assert_eq!(
            validate_effect_caption(&script, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_script_rejected"
        );

        let handler = compose(
            r#"<div class="x" onclick="run()">这个价格真的绷不住了</div>"#,
            ".x{opacity:1}",
        );
        assert_eq!(
            validate_effect_caption(&handler, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_event_handler_rejected"
        );
    }

    #[test]
    fn rejects_external_reference() {
        let external = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{background-image:url(https://example.com/a.png)}",
        );
        assert_eq!(
            validate_effect_caption(&external, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_external_reference_rejected"
        );

        let linked = compose(
            r#"<div class="x" href="https://example.com">这个价格真的绷不住了</div>"#,
            "",
        );
        assert_eq!(
            validate_effect_caption(&linked, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_external_reference_rejected"
        );
    }

    #[test]
    fn rejects_engine_script_injection() {
        // 组合试图自带"引擎脚本"：即使名字像渲染器的，也必须拒绝，页面可执行内容只能来自随包渲染器。
        let injected = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{opacity:1}",
        )
        .replace(
            "<body>",
            "<body>\n<script data-hyperframes-preview-runtime=\"1\">/* forged */</script>",
        );
        assert_eq!(
            validate_effect_caption(&injected, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_script_rejected"
        );
    }

    #[test]
    fn rejects_unsupported_css_capability() {
        let at_import = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            "@import \"theme.css\";",
        );
        assert_eq!(
            validate_effect_caption(&at_import, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_external_reference_rejected"
        );

        let media = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            "@media (prefers-color-scheme: dark) { .x { opacity: 1; } }",
        );
        assert_eq!(
            validate_effect_caption(&media, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_unsupported_css_property"
        );

        let unknown_property = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{behavior:url(#default#time2)}",
        );
        assert_eq!(
            validate_effect_caption(&unknown_property, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_external_reference_rejected"
        );
    }

    #[test]
    fn rejects_limits_exceeded() {
        let long_animation = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{animation-duration:120s}",
        );
        assert_eq!(
            validate_effect_caption(&long_animation, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_limits_exceeded"
        );

        let deep = compose(
            &format!(
                "{}这个价格真的绷不住了{}",
                "<div>".repeat(30),
                "</div>".repeat(30)
            ),
            ".x{opacity:1}",
        );
        assert_eq!(
            validate_effect_caption(&deep, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_limits_exceeded"
        );

        let too_many = compose(
            &format!("{}这个价格真的绷不住了", "<span></span>".repeat(600)),
            ".x{opacity:1}",
        );
        assert_eq!(
            validate_effect_caption(&too_many, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_limits_exceeded"
        );
    }

    #[test]
    fn rejects_text_outside_published_corpus() {
        let invented = compose(r#"<div class="x">恭喜发财红包拿来</div>"#, ".x{opacity:1}");
        assert_eq!(
            validate_effect_caption(&invented, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_text_reference_rejected"
        );

        let generated = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x::after{content:\"编造的字幕\"}",
        );
        assert_eq!(
            validate_effect_caption(&generated, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_text_reference_rejected"
        );
    }

    #[test]
    fn rejects_opaque_full_screen_background() {
        let opaque = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            "html, body { background-color:#000000; }",
        );
        assert_eq!(
            validate_effect_caption(&opaque, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_opaque_background_rejected"
        );

        let inline = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{opacity:1}",
        )
        .replace("<body>", "<body style=\"background:#111\">");
        assert_eq!(
            validate_effect_caption(&inline, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_opaque_background_rejected"
        );
    }

    #[test]
    fn rejects_nondeterministic_timing() {
        let random = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{transform:translateY(random(0px, 40px))}",
        );
        assert_eq!(
            validate_effect_caption(&random, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_nondeterministic_rejected"
        );

        let scroll_driven = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{animation-timeline:scroll()}",
        );
        assert_eq!(
            validate_effect_caption(&scroll_driven, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_nondeterministic_rejected"
        );
    }

    #[test]
    fn rejects_build_configuration() {
        let build = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{opacity:1}",
        )
        .replace("<body>", "<body>\n<!-- npm install hyperframes -->");
        assert_eq!(
            validate_effect_caption(&build, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_script_rejected"
        );
    }

    #[test]
    fn rejects_malformed_markup() {
        let unclosed = compose(r#"<div class="x">这个价格真的绷不住了"#, ".x{opacity:1}");
        assert_eq!(
            validate_effect_caption(&unclosed, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_caption_malformed"
        );

        let missing_contract = compliant().replace(r#" data-duration="15""#, "");
        assert_eq!(
            validate_effect_caption(&missing_contract, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_caption_malformed"
        );

        let wrong_canvas = compliant().replace(r#"data-width="1080""#, r#"data-width="720""#);
        assert_eq!(
            validate_effect_caption(&wrong_canvas, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_caption_malformed"
        );
    }

    #[test]
    fn rejection_detail_never_echoes_composition_text() {
        let invented = compose(r#"<div class="x">恭喜发财红包拿来</div>"#, ".x{opacity:1}");
        let rejection = validate_effect_caption(&invented, &corpus(), &LIMITS).unwrap_err();
        assert!(
            !rejection.detail.contains("恭喜发财"),
            "拒绝原因不得回显组合正文：{}",
            rejection.detail
        );
        let external = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{background-image:url(https://example.com/a.png)}",
        );
        let rejection = validate_effect_caption(&external, &corpus(), &LIMITS).unwrap_err();
        assert!(
            !rejection.detail.contains("example.com"),
            "拒绝原因不得回显 URL：{}",
            rejection.detail
        );
    }

    #[test]
    fn font_family_must_be_bundled() {
        let system_font = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{font-family:\"PingFang SC\", sans-serif}",
        );
        assert_eq!(
            validate_effect_caption(&system_font, &corpus(), &LIMITS)
                .unwrap_err()
                .code,
            "effect_text_reference_rejected"
        );

        let bundled = compose(
            r#"<div class="x">这个价格真的绷不住了</div>"#,
            ".x{font-family:\"hyperframes-caption-cjk\"}",
        );
        assert!(validate_effect_caption(&bundled, &corpus(), &LIMITS).is_ok());
    }
}
