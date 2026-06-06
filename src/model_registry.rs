#[path = "model_registry_data.rs"]
mod model_registry_data;

use std::collections::HashMap;
use std::sync::OnceLock;

fn exact_table() -> &'static HashMap<&'static str, u64> {
    static MAP: OnceLock<HashMap<&str, u64>> = OnceLock::new();
    MAP.get_or_init(|| model_registry_data::EXACT.iter().copied().collect())
}

pub fn model_context_window(model: &str) -> Option<u64> {
    if model.is_empty() {
        return None;
    }

    let lower = model.to_lowercase();

    if let Some(&ctx) = exact_table().get(lower.as_str()) {
        return Some(ctx);
    }

    for &(prefix, ctx) in model_registry_data::PREFIXES {
        if lower.starts_with(prefix) {
            return Some(ctx);
        }
    }

    if let Some(pos) = lower.rfind('/') {
        let stripped = &lower[pos + 1..];
        if !stripped.is_empty() {
            if let Some(&ctx) = exact_table().get(stripped) {
                return Some(ctx);
            }

            for &(prefix, ctx) in model_registry_data::PREFIXES {
                if stripped.starts_with(prefix) {
                    return Some(ctx);
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_models() {
        assert_eq!(model_context_window("claude-2.0"), Some(100_000));
        assert_eq!(model_context_window("claude-3-5-sonnet"), Some(200_000));
        assert_eq!(model_context_window("claude-3-5-sonnet-20240620"), Some(200_000));
        assert_eq!(model_context_window("claude-opus-4"), Some(200_000));
        assert_eq!(model_context_window("claude-sonnet-4"), Some(200_000));
        assert_eq!(model_context_window("claude-sonnet-4-20250514"), Some(1_000_000));
    }

    #[test]
    fn test_openai_models() {
        assert_eq!(model_context_window("gpt-4"), Some(8_191));
        assert_eq!(model_context_window("gpt-4-turbo"), Some(128_000));
        assert_eq!(model_context_window("gpt-4o"), Some(128_000));
        assert_eq!(model_context_window("gpt-4.1"), Some(1_047_576));
        assert_eq!(model_context_window("o1"), Some(200_000));
        assert_eq!(model_context_window("o3"), Some(200_000));
        assert_eq!(model_context_window("o4-mini"), Some(200_000));
    }

    #[test]
    fn test_deepseek_models() {
        assert_eq!(model_context_window("deepseek-chat"), Some(65_536));
        assert_eq!(model_context_window("deepseek-reasoner"), Some(131_072));
    }

    #[test]
    fn test_xai_models() {
        assert_eq!(model_context_window("grok-2"), Some(131_072));
        assert_eq!(model_context_window("grok-3"), Some(131_072));
    }

    #[test]
    fn test_google_models() {
        assert_eq!(model_context_window("gemini-2.5-pro"), Some(1_000_000));
        assert_eq!(model_context_window("gemini-2.5-flash"), Some(1_000_000));
        assert_eq!(model_context_window("gemini-1.5-pro"), Some(2_000_000));
        assert_eq!(model_context_window("gemini-1.5-flash"), Some(1_000_000));
    }

    #[test]
    fn test_qwen_models() {
        assert_eq!(model_context_window("qwen-3.7-max"), Some(1_000_000));
        assert_eq!(model_context_window("qwen3.7-max"), Some(1_000_000));
        assert_eq!(model_context_window("qwen3.5-plus"), Some(991_808));
        assert_eq!(model_context_window("qwen3.6-plus"), Some(1_000_000));
        assert_eq!(model_context_window("qwen-max"), Some(131_072));
        assert_eq!(model_context_window("qwen-plus"), Some(131_072));
    }

    #[test]
    fn test_llama_models() {
        assert_eq!(model_context_window("llama-3.1-70b"), Some(128_000));
        assert_eq!(model_context_window("llama-3.2-11b"), Some(128_000));
    }

    #[test]
    fn test_empty_string() {
        assert_eq!(model_context_window(""), None);
    }

    #[test]
    fn test_unknown_model() {
        assert_eq!(model_context_window("nonexistent-model-xyz"), None);
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(model_context_window("CLAUDE-3-5-SONNET"), Some(200_000));
        assert_eq!(model_context_window("GPT-4O"), Some(128_000));
        assert_eq!(model_context_window("Qwen-3.7-Max"), Some(1_000_000));
    }

    #[test]
    fn test_provider_prefix_stripping() {
        assert_eq!(model_context_window("bailian-payg/qwen3.7-max"), Some(1_000_000));
        assert_eq!(model_context_window("openrouter/anthropic/claude-sonnet-4"), Some(200_000));
        assert_eq!(model_context_window("dashscope/qwen-max"), Some(131_072));
        assert_eq!(model_context_window("openai/gpt-4o"), Some(128_000));
    }
}
