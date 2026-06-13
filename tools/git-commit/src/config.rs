use serde::Deserialize;
use std::path::PathBuf;

/// Resolved tool configuration.
/// Priority (highest to lowest):
///   CLI flag → env var → config file → built-in default
#[derive(Debug, Default)]
pub struct Config {
    pub anthropic_model: Option<String>,
    pub ollama_model: Option<String>,
    pub ollama_url: Option<String>,
}

/// Raw TOML shape for `$XDG_CONFIG_HOME/tpx-ai/config.toml`
#[derive(Debug, Default, Deserialize)]
struct TomlConfig {
    anthropic: Option<TomlAnthropic>,
    ollama: Option<TomlOllama>,
}

#[derive(Debug, Deserialize)]
struct TomlAnthropic {
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlOllama {
    model: Option<String>,
    url: Option<String>,
}

impl Config {
    /// Load config from `$XDG_CONFIG_HOME/tpx-ai/config.toml` (if it exists).
    /// Env vars are read separately by clap; this only covers the file layer.
    pub fn load() -> Self {
        let file = config_path();
        let toml: TomlConfig = file
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default();

        Config {
            anthropic_model: toml.anthropic.and_then(|a| a.model),
            ollama_model: toml.ollama.as_ref().and_then(|o| o.model.clone()),
            ollama_url: toml.ollama.and_then(|o| o.url),
        }
    }
}

fn config_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("tpx-ai").join("config.toml"))
}
