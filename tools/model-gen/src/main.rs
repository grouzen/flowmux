use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

const LITELLM_URL: &str = "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

#[derive(Deserialize, Debug)]
struct ModelEntry {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    litellm_provider: Option<String>,
    #[serde(default)]
    max_input_tokens: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
}

const RELEVANT_MODES: &[&str] = &["chat", "completion"];

const MANUAL_OVERRIDES: &[(&str, u64)] = &[
    ("claude-3-5-sonnet-20240620", 200_000),
    ("claude-3-5-sonnet", 200_000),
    ("qwen-3.7-max", 1_000_000),
    ("qwen3.7-max", 1_000_000),
    ("qwen-max", 131_072),
    ("qwen-plus", 131_072),
];

const ALL_PREFIXES: &[&str] = &[
    "openrouter/",
    "together_ai/",
    "fireworks_ai/",
    "dashscope/",
    "deepinfra/",
    "groq/",
    "cerebras/",
    "perplexity/",
    "replicate/",
    "huggingface/",
    "ollama/",
    "github/",
    "azure/",
    "cloudflare/",
    "bedrock/",
    "bedrock_converse/",
    "vertex_ai/",
    "anthropic/",
    "openai/",
    "google/",
    "gemini/",
    "xai/",
    "deepseek/",
    "deepseek-ai/",
    "mistral/",
    "meta/",
    "cohere/",
    "ai21/",
    "amazon/",
    "writer/",
    "qwen/",
    "nvidia/",
    "minimax/",
    "moonshot/",
    "moonshotai/",
    "zai/",
    "novita/",
    "nebius/",
    "hyperbolic/",
    "nscale/",
    "sambanova/",
    "lemonade/",
    "llamagate/",
    "lambda_ai/",
    "gradient_ai/",
    "gmi/",
    "crusoe/",
    "wandb/",
    "publicai/",
    "vercel_ai_gateway/",
    "ovhcloud/",
    "aiml/",
];

const REGION_PREFIXES: &[&str] = &[
    "us.", "eu.", "ap.", "global.", "apac.", "au.", "us-gov.", "jp.",
];

fn fetch_litellm_json() -> Value {
    eprintln!("Fetching LiteLLM model database from {}...", LITELLM_URL);
    let resp = reqwest::blocking::get(LITELLM_URL)
        .expect("Failed to fetch LiteLLM JSON");
    let text = resp.text().expect("Failed to read response body");
    serde_json::from_str(&text).expect("Failed to parse JSON")
}

fn strip_provider_prefixes(key: &str) -> String {
    let mut s = key.to_string();
    loop {
        let before = s.clone();
        for prefix in REGION_PREFIXES {
            if let Some(stripped) = s.strip_prefix(prefix) {
                s = stripped.to_string();
            }
        }
        for prefix in ALL_PREFIXES {
            if let Some(stripped) = s.strip_prefix(prefix) {
                s = stripped.to_string();
            }
        }
        if let Some(stripped) = s.strip_prefix("anthropic.") {
            s = stripped.to_string();
        }
        if s == before {
            break;
        }
    }
    s
}

fn strip_bedrock_suffixes(key: &str) -> String {
    let mut s = key.to_string();

    let re_v = regex::Regex::new(r"-v\d+:\d+$").unwrap();
    if let Some(m) = re_v.find(&s) {
        s = s[..m.start()].to_string();
    }

    let re_v2 = regex::Regex::new(r"-v\d+$").unwrap();
    if let Some(m) = re_v2.find(&s) {
        s = s[..m.start()].to_string();
    }

    let re_at = regex::Regex::new(r"@.+$").unwrap();
    if let Some(m) = re_at.find(&s) {
        s = s[..m.start()].to_string();
    }

    s
}

fn generate_canonical_names(raw_key: &str) -> Vec<String> {
    let mut names = std::collections::HashSet::new();
    names.insert(raw_key.to_string());

    let stripped = strip_provider_prefixes(raw_key);
    names.insert(stripped.clone());

    let stripped_lower = stripped.to_lowercase();
    if stripped_lower != stripped {
        names.insert(stripped_lower.clone());
    }

    let clean = strip_bedrock_suffixes(&stripped);
    if clean != stripped {
        names.insert(clean.clone());
    }

    let clean_lower = strip_bedrock_suffixes(&stripped_lower);
    if clean_lower != stripped_lower {
        names.insert(clean_lower);
    }

    names.into_iter().collect()
}

fn extract_model_entries(data: &Value) -> HashMap<String, u64> {
    let mut entries: HashMap<String, u64> = HashMap::new();
    let obj = data.as_object().expect("Expected JSON object");

    for (key, value) in obj {
        if key == "sample_spec" {
            continue;
        }

        let entry: ModelEntry = match serde_json::from_value(value.clone()) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let mode = entry.mode.as_deref().unwrap_or("");
        if !RELEVANT_MODES.contains(&mode) {
            continue;
        }

        let context_window = entry.max_input_tokens.or(entry.max_tokens);

        let context_window = match context_window {
            Some(v) if v > 0 => v,
            _ => continue,
        };

        let canonical_names = generate_canonical_names(key);

        for name in canonical_names {
            if name.contains('/') || name.contains('\\') {
                continue;
            }
            let entry = entries.entry(name).or_insert(context_window);
            if context_window < *entry {
                *entry = context_window;
            }
        }
    }

    entries.retain(|k, _| !k.contains('/') && k.len() >= 4);

    entries
}

fn build_prefix_table(exact: &HashMap<String, u64>) -> Vec<(String, u64)> {
    let mut prefix_groups: HashMap<String, u64> = HashMap::new();

    for (key, &value) in exact {
        if key.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
            prefix_groups.entry(key.clone()).or_insert(value);
        }
    }

    let model_families: &[(&str, u64)] = &[
        ("claude-opus-4-8", 1_000_000),
        ("claude-opus-4-7", 1_000_000),
        ("claude-opus-4-6", 1_000_000),
        ("claude-sonnet-4-6", 1_000_000),
        ("claude-opus-4-5", 200_000),
        ("claude-sonnet-4-5", 200_000),
        ("claude-haiku-4-5", 200_000),
        ("claude-opus-4", 200_000),
        ("claude-sonnet-4", 200_000),
        ("claude-3-7-sonnet", 200_000),
        ("claude-3-5-sonnet", 200_000),
        ("claude-3-5-haiku", 200_000),
        ("claude-3-opus", 200_000),
        ("claude-3-sonnet", 200_000),
        ("claude-3-haiku", 200_000),
        ("claude-2", 100_000),
        ("claude-instant", 100_000),
        ("claude", 200_000),
        ("gpt-4o", 128_000),
        ("gpt-4-turbo", 128_000),
        ("gpt-4.1", 1_000_000),
        ("gpt-4.5", 128_000),
        ("gpt-4", 8_000),
        ("gpt-3.5-turbo", 16_000),
        ("o1-pro", 200_000),
        ("o1-mini", 128_000),
        ("o1-preview", 128_000),
        ("o1", 200_000),
        ("o3", 200_000),
        ("o4-mini", 200_000),
        ("gemini-2.5-pro", 1_000_000),
        ("gemini-2.5-flash", 1_000_000),
        ("gemini-2.0-flash", 1_000_000),
        ("gemini-1.5-pro", 2_000_000),
        ("gemini-1.5-flash", 1_000_000),
        ("gemini-pro", 32_000),
        ("grok-3", 131_072),
        ("grok-2", 131_072),
        ("deepseek-reasoner", 131_072),
        ("deepseek-chat", 131_072),
        ("deepseek-r1", 128_000),
        ("deepseek-v3", 128_000),
        ("mistral-large", 128_000),
        ("mistral-medium", 128_000),
        ("mistral-small", 32_000),
        ("codestral", 32_000),
        ("mixtral", 32_000),
        ("qwen3.7-max", 1_000_000),
        ("qwen3.6-plus", 1_000_000),
        ("qwen3.5", 262_144),
        ("qwen3-coder", 262_144),
        ("qwen3-vl", 128_000),
        ("qwen3-next", 262_144),
        ("qwen3-max", 258_048),
        ("qwen3", 131_072),
        ("qwen-max", 131_072),
        ("qwen-plus", 131_072),
        ("qwen-turbo", 1_000_000),
        ("qwen-coder", 1_000_000),
        ("qwen-flash", 997_952),
        ("llama-4-maverick", 1_000_000),
        ("llama-4-scout", 10_000_000),
        ("llama-3.3", 128_000),
        ("llama-3.2", 128_000),
        ("llama-3.1", 128_000),
        ("llama-3", 128_000),
        ("nova-2-pro", 1_000_000),
        ("nova-2-lite", 1_000_000),
        ("nova-pro", 300_000),
        ("nova-lite", 300_000),
        ("nova-micro", 128_000),
        ("jamba-1.5", 256_000),
        ("command-r-plus", 128_000),
        ("command-r", 128_000),
        ("kimi-k2", 262_144),
        ("minimax-m2", 128_000),
    ];

    for (prefix, ctx) in model_families {
        prefix_groups.insert(prefix.to_string(), *ctx);
    }

    let mut prefixes: Vec<(String, u64)> = prefix_groups.into_iter().collect();
    prefixes.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then(b.0.cmp(&a.0)));

    prefixes
}

fn generate_rust_code(
    exact: &HashMap<String, u64>,
    prefixes: &[(String, u64)],
) -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");

    let mut exact_entries: Vec<_> = exact.iter().collect();
    exact_entries.sort_by(|a, b| a.0.cmp(b.0));

    let exact_lines: Vec<String> = exact_entries
        .iter()
        .map(|(name, ctx)| format!("    (\"{}\", {}),", escape_str(name), ctx))
        .collect();

    let prefix_lines: Vec<String> = prefixes
        .iter()
        .map(|(name, ctx)| format!("    (\"{}\", {}),", escape_str(name), ctx))
        .collect();

    format!(
        r#"// AUTO-GENERATED by tools/model-gen — do not edit manually.
// Source: litellm/model_prices_and_context_window.json
// Generated: {}
// Entries: exact={}, prefixes={}

/// Exact-match table: canonical model ID -> context window tokens.
pub(crate) static EXACT: &[(&str, u64)] = &[
{}
];

/// Prefix-match table: sorted by prefix length descending (longest first).
/// Used as fallback when exact match fails.
pub(crate) static PREFIXES: &[(&str, u64)] = &[
{}
];
"#,
        now,
        exact_entries.len(),
        prefixes.len(),
        exact_lines.join("\n"),
        prefix_lines.join("\n"),
    )
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn main() {
    let data = fetch_litellm_json();

    eprintln!("Processing model entries (all providers)...");
    let mut entries = extract_model_entries(&data);

    for (name, ctx) in MANUAL_OVERRIDES {
        entries.insert(name.to_string(), *ctx);
    }

    eprintln!("Building prefix table...");
    let prefixes = build_prefix_table(&entries);

    eprintln!("Generating Rust code...");
    let code = generate_rust_code(&entries, &prefixes);

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output_path = manifest_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("src")
        .join("model_registry_data.rs");

    fs::write(&output_path, &code).expect("Failed to write output file");

    eprintln!("Generated {} entries", entries.len());
    eprintln!("Generated {} prefixes", prefixes.len());
    eprintln!("Output written to: {}", output_path.display());

    let provider_counts: HashMap<&str, usize> = entries
        .keys()
        .filter_map(|k| {
            if k.starts_with("claude") {
                Some("anthropic")
            } else if k.starts_with("gpt") || k.starts_with("o1") || k.starts_with("o3") || k.starts_with("o4") {
                Some("openai")
            } else if k.starts_with("gemini") {
                Some("google")
            } else if k.starts_with("grok") {
                Some("xai")
            } else if k.starts_with("deepseek") {
                Some("deepseek")
            } else if k.starts_with("mistral") || k.starts_with("codestral") || k.starts_with("mixtral") {
                Some("mistral")
            } else if k.starts_with("qwen") {
                Some("qwen")
            } else if k.starts_with("llama") {
                Some("meta")
            } else if k.starts_with("nova") {
                Some("amazon")
            } else if k.starts_with("jamba") {
                Some("ai21")
            } else if k.starts_with("command") {
                Some("cohere")
            } else if k.starts_with("kimi") {
                Some("moonshot")
            } else {
                None
            }
        })
        .fold(HashMap::new(), |mut acc, p| {
            *acc.entry(p).or_insert(0) += 1;
            acc
        });

    let mut provider_counts: Vec<_> = provider_counts.into_iter().collect();
    provider_counts.sort_by(|a, b| b.1.cmp(&a.1));

    eprintln!("\nProvider coverage:");
    for (provider, count) in &provider_counts {
        eprintln!("  {}: {} models", provider, count);
    }
}
