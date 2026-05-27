use std::collections::HashMap;
use std::sync::OnceLock;

static PLACEHOLDER_MODEL_MAP: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();

fn get_placeholder_map() -> &'static HashMap<&'static str, &'static str> {
    PLACEHOLDER_MODEL_MAP.get_or_init(|| {
        let mut m = HashMap::new();
        // Gemini 3.1 Pro (formerly mapped to 3.5 Pro placeholder)
        m.insert("MODEL_PLACEHOLDER_M16", "gemini-3.1-pro-high");

        // Gemini 3.5 Flash
        m.insert("MODEL_PLACEHOLDER_M133", "gemini-3.5-flash");
        m.insert("gemini-3-flash-b", "gemini-3.5-flash");
        m.insert("gemini-3-flash-agent", "gemini-3.5-flash");
        m.insert("MODEL_PLACEHOLDER_M187", "gemini-3.5-flash");
        m.insert("MODEL_PLACEHOLDER_M20", "gemini-3.5-flash");

        // Gemini 3.1 Pro
        m.insert("MODEL_PLACEHOLDER_M37", "gemini-3.1-pro-high");
        m.insert("MODEL_PLACEHOLDER_M36", "gemini-3.1-pro-low");
        m.insert("gemini-3.1-pro-low", "gemini-3.1-pro-low");
        m.insert("gemini-3.1-pro-high", "gemini-3.1-pro-high");

        // Gemini 3 Flash
        m.insert("MODEL_PLACEHOLDER_M18", "gemini-3-flash");

        // Gemini 3 Pro
        m.insert("MODEL_PLACEHOLDER_M8", "gemini-3-pro-high");
        m.insert("MODEL_PLACEHOLDER_M7", "gemini-3-pro-low");

        // Gemini 3 Pro Image
        m.insert("MODEL_PLACEHOLDER_M9", "gemini-3-pro-image");

        // Claude Opus 4.6 (Thinking)
        m.insert("MODEL_PLACEHOLDER_M26", "claude-opus-4-6-thinking");
        m.insert("claude-opus-4-6-thinking", "claude-opus-4-6-thinking");

        // Claude Sonnet 4.6 (Thinking)
        m.insert("MODEL_PLACEHOLDER_M35", "claude-sonnet-4-6-thinking");
        m.insert("claude-sonnet-4-6-thinking", "claude-sonnet-4-6-thinking");

        // Claude Opus 4.5 (Thinking)
        m.insert("MODEL_PLACEHOLDER_M12", "claude-opus-4-5-thinking");

        // Non-placeholder internal IDs
        m.insert("MODEL_OPENAI_GPT_OSS_120B_MEDIUM", "gpt-oss-120b-medium");
        m.insert("MODEL_CLAUDE_4_5_SONNET", "claude-sonnet-4-5");
        m.insert("MODEL_CLAUDE_4_5_SONNET_THINKING", "claude-sonnet-4-5-thinking");
        m.insert("MODEL_GOOGLE_GEMINI_2_5_FLASH", "gemini-2.5-flash");
        m.insert("MODEL_GOOGLE_GEMINI_2_5_FLASH_LITE", "gemini-2.5-flash-lite");
        m
    })
}

pub fn resolve_model_placeholder(value: &str) -> Option<&'static str> {
    get_placeholder_map().get(value).copied()
}

pub fn preferred_model(candidates: &[Option<&str>], inherited_model: Option<&str>) -> Option<String> {
    for &opt in candidates {
        if let Some(candidate) = opt {
            let candidate = candidate.trim();
            if !candidate.is_empty() {
                if let Some(resolved) = resolve_model_placeholder(candidate) {
                    return Some(resolved.to_string());
                }
                if !candidate.starts_with("MODEL_PLACEHOLDER_") {
                    return Some(candidate.to_string());
                }
            }
        }
    }

    if let Some(inherited) = inherited_model {
        let inherited = inherited.trim();
        if !inherited.is_empty() {
            return Some(resolve_model_placeholder(inherited).map(|s| s.to_string()).unwrap_or_else(|| inherited.to_string()));
        }
    }

    // fallback to first non-empty candidate
    for &opt in candidates {
        if let Some(candidate) = opt {
            let candidate = candidate.trim();
            if !candidate.is_empty() {
                return Some(resolve_model_placeholder(candidate).map(|s| s.to_string()).unwrap_or_else(|| candidate.to_string()));
            }
        }
    }

    None
}
