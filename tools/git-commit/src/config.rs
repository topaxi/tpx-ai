use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum CommitFormat {
    #[default]
    Conventional,
    Scoped,
}

impl std::fmt::Display for CommitFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommitFormat::Conventional => write!(f, "conventional"),
            CommitFormat::Scoped => write!(f, "scoped"),
        }
    }
}

impl std::str::FromStr for CommitFormat {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "conventional" => Ok(CommitFormat::Conventional),
            "scoped" => Ok(CommitFormat::Scoped),
            _ => Err(format!(
                "unknown commit format '{s}' — expected 'conventional' or 'scoped'"
            )),
        }
    }
}

/// Resolved tool configuration.
/// Priority (highest to lowest):
///   CLI flag → env var → matching [[projects]] entry → global section → built-in default
#[derive(Debug, Default)]
pub struct Config {
    pub anthropic_model: Option<String>,
    pub ollama_model: Option<String>,
    pub ollama_url: Option<String>,
    pub commit_format: Option<CommitFormat>,
}

/// Raw TOML shape for `$XDG_CONFIG_HOME/tpx-ai/config.toml`.
///
/// ```toml
/// [commit]
/// format = "conventional"
///
/// [[projects]]
/// path = "~/work/myrepo"
/// commit.format = "scoped"
///
/// [[projects]]
/// path = "$WORK/other"
/// ollama.model = "gemma4:12b"
/// ```
#[derive(Debug, Default, Deserialize)]
struct TomlConfig {
    anthropic: Option<TomlAnthropic>,
    ollama: Option<TomlOllama>,
    commit: Option<TomlCommit>,
    projects: Option<Vec<TomlProjectEntry>>,
}

#[derive(Debug, Clone, Deserialize)]
struct TomlProjectEntry {
    path: String,
    anthropic: Option<TomlAnthropic>,
    ollama: Option<TomlOllama>,
    commit: Option<TomlCommit>,
}

#[derive(Debug, Clone, Deserialize)]
struct TomlAnthropic {
    model: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TomlOllama {
    model: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct TomlCommit {
    format: Option<CommitFormat>,
}

impl Config {
    pub fn load() -> Self {
        let toml = load_toml(global_config_path());

        let proj = git_root().and_then(|root| {
            toml.projects
                .as_ref()?
                .iter()
                .find(|p| expand_path(&p.path) == root)
        });

        Config {
            anthropic_model: proj
                .and_then(|p| p.anthropic.as_ref()?.model.clone())
                .or_else(|| toml.anthropic.as_ref()?.model.clone()),
            ollama_model: proj
                .and_then(|p| p.ollama.as_ref()?.model.clone())
                .or_else(|| toml.ollama.as_ref()?.model.clone()),
            ollama_url: proj
                .and_then(|p| p.ollama.as_ref()?.url.clone())
                .or_else(|| toml.ollama.as_ref()?.url.clone()),
            commit_format: proj
                .and_then(|p| p.commit.as_ref()?.format)
                .or_else(|| toml.commit.as_ref()?.format),
        }
    }
}

fn load_toml(path: Option<PathBuf>) -> TomlConfig {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

fn global_config_path() -> Option<PathBuf> {
    let base = dirs::config_dir()?;
    Some(base.join("tpx-ai").join("config.toml"))
}

/// Walk up from the current directory to find the git root.
/// No subprocess — safe to call while git holds locks.
fn git_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".git").exists() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Expand `~` and `$VAR` / `${VAR}` in a path string.
fn expand_path(s: &str) -> PathBuf {
    let s = if s == "~" || s.starts_with("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        format!("{}{}", home, &s[1..])
    } else {
        s.to_string()
    };

    let mut result = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(dollar) = rest.find('$') {
        result.push_str(&rest[..dollar]);
        rest = &rest[dollar + 1..];
        let (var_name, after) = if rest.starts_with('{') {
            let end = rest.find('}').unwrap_or(rest.len());
            (&rest[1..end], &rest[(end + 1).min(rest.len())..])
        } else {
            let end = rest
                .find(|c: char| !c.is_alphanumeric() && c != '_')
                .unwrap_or(rest.len());
            (&rest[..end], &rest[end..])
        };
        if let Ok(val) = std::env::var(var_name) {
            result.push_str(&val);
        }
        rest = after;
    }
    result.push_str(rest);
    PathBuf::from(result)
}
