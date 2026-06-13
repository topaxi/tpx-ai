mod config;

use anyhow::{bail, Context, Result};
use clap::Parser;
use config::{CommitFormat, Config};
use llm::{LlmProvider, Message};
use std::io::Write as _;
use std::process::Command;

#[derive(Parser)]
#[command(
    name = "git-commit",
    about = "Generate and create git commits using AI",
    version
)]
struct Cli {
    /// Only include files matching these glob patterns.
    /// Can be repeated or comma-separated: --include='src/*.rs' --include='Cargo.toml'
    #[arg(long = "include", value_delimiter = ',')]
    include: Vec<String>,

    /// Additional context to guide the commit message
    #[arg(long = "context", short = 'c')]
    context: Option<String>,

    /// LLM provider: anthropic or ollama
    #[arg(long, default_value = "anthropic")]
    provider: String,

    /// Model to use — overrides ANTHROPIC_MODEL / OLLAMA_MODEL and config file
    #[arg(long, short = 'm')]
    model: Option<String>,

    /// Anthropic API key [env: ANTHROPIC_API_KEY]
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    anthropic_api_key: Option<String>,

    /// Anthropic model [env: ANTHROPIC_MODEL] [config: anthropic.model]
    #[arg(long, env = "ANTHROPIC_MODEL")]
    anthropic_model: Option<String>,

    /// Ollama model [env: OLLAMA_MODEL] [config: ollama.model]
    #[arg(long, env = "OLLAMA_MODEL")]
    ollama_model: Option<String>,

    /// Ollama base URL [env: OLLAMA_HOST] [config: ollama.url]
    #[arg(long, env = "OLLAMA_HOST")]
    ollama_url: Option<String>,

    /// Commit message format: "conventional" (type(scope): desc) or "scoped" (scope: desc)
    /// [env: GIT_COMMIT_FORMAT] [config: commit.format]
    #[arg(long)]
    commit_format: Option<String>,

    /// Print the generated commit message without committing
    #[arg(long)]
    dry_run: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let diff = staged_diff()?;
    if diff.is_empty() {
        bail!("no staged changes — run `git add` first");
    }

    let diff = if cli.include.is_empty() {
        diff
    } else {
        filter_diff(&diff, &cli.include)
    };

    if diff.trim().is_empty() {
        bail!("no staged changes match the --include patterns");
    }

    let cfg = Config::load();

    // Priority: --commit-format > project config > global config > default
    let format = cli
        .commit_format
        .as_deref()
        .map(|s| s.parse::<CommitFormat>().map_err(|e| anyhow::anyhow!(e)))
        .transpose()?
        .or(cfg.commit_format)
        .unwrap_or_default();

    let branch = current_branch();
    let provider = build_provider(&cli, &cfg).await?;
    let file_diffs = split_into_file_diffs(&diff);
    if file_diffs.is_empty() {
        bail!("failed to parse any file diffs — this is a bug");
    }

    // Dry-run: stream body lines to stdout as each file completes, subject last.
    // The plugin reads these progressively to update virtual text.
    if cli.dry_run {
        eprintln!("summarizing {} file(s)…", file_diffs.len());
        let mut summaries = Vec::with_capacity(file_diffs.len());
        for (path, content) in &file_diffs {
            eprint!("  {path}…");
            let summary = summarize_file_diff(path, content, &provider)
                .await
                .with_context(|| format!("failed to summarize {path}"))?;
            eprintln!(" {summary}");
            summaries.push(format!("{path}: {summary}"));
            println!("- {path}: {summary}");
            std::io::stdout().flush().ok();
        }
        eprintln!("generating subject…");
        let subject = generate_subject(&summaries, cli.context.as_deref(), format, cfg.commit_prompt_extra.as_deref(), branch.as_deref(), &provider).await?;
        println!("{}", subject.trim());
        return Ok(());
    }

    let summaries = summarize_all(&file_diffs, &provider).await?;
    let subject = generate_subject(&summaries, cli.context.as_deref(), format, cfg.commit_prompt_extra.as_deref(), branch.as_deref(), &provider).await?;
    let body = summaries
        .iter()
        .map(|s| format!("- {s}"))
        .collect::<Vec<_>>()
        .join("\n");
    let message = format!("{}\n\n{}", subject.trim(), body).trim().to_string();

    run_commit(&message)?;
    println!("✓ {message}");

    Ok(())
}

// ── git ──────────────────────────────────────────────────────────────────────

fn current_branch() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?;
    let branch = branch.trim();
    (branch != "HEAD").then(|| branch.to_string())
}

fn staged_diff() -> Result<String> {
    let out = Command::new("git")
        .args(["diff", "--staged", "--no-ext-diff"])
        .output()
        .context("failed to run `git diff --staged`")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("`git diff --staged` failed: {stderr}");
    }

    String::from_utf8(out.stdout).context("git diff output is not valid UTF-8")
}

fn run_commit(message: &str) -> Result<()> {
    let out = Command::new("git")
        .args(["commit", "-m", message])
        .output()
        .context("failed to run `git commit`")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("`git commit` failed: {stderr}");
    }

    Ok(())
}

// ── diff parsing ──────────────────────────────────────────────────────────────

/// Split a unified diff into `(file_path, diff_content)` pairs.
fn split_into_file_diffs(diff: &str) -> Vec<(String, String)> {
    let mut result: Vec<(String, String)> = Vec::new();

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            let path = rest.split_once(" b/").map_or(rest, |(p, _)| p);
            result.push((path.to_string(), String::new()));
        }
        if let Some((_, content)) = result.last_mut() {
            content.push_str(line);
            content.push('\n');
        }
    }

    result
}

/// Return only the file diffs whose paths match at least one pattern.
fn filter_diff(diff: &str, patterns: &[String]) -> String {
    split_into_file_diffs(diff)
        .into_iter()
        .filter(|(path, _)| matches_any(path, patterns))
        .map(|(_, content)| content)
        .collect()
}

fn matches_any(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pat| {
        glob::Pattern::new(pat)
            .map(|p| p.matches(path))
            .unwrap_or_else(|_| path.contains(pat.as_str()))
    })
}

// ── LLM calls ────────────────────────────────────────────────────────────────

/// Maximum bytes to send for a single file summary; larger diffs are truncated.
const MAX_FILE_DIFF_BYTES: usize = 4_000;

/// Summarize every file diff sequentially, printing progress to stderr.
async fn summarize_all(
    file_diffs: &[(String, String)],
    provider: &LlmProvider,
) -> Result<Vec<String>> {
    eprintln!("summarizing {} file(s)…", file_diffs.len());
    let mut summaries = Vec::with_capacity(file_diffs.len());

    for (path, content) in file_diffs {
        eprint!("  {path}…");
        let summary = summarize_file_diff(path, content, provider).await?;
        eprintln!(" {summary}");
        summaries.push(format!("{path}: {summary}"));
    }

    Ok(summaries)
}

async fn summarize_file_diff(path: &str, diff: &str, provider: &LlmProvider) -> Result<String> {
    let diff = if diff.len() > MAX_FILE_DIFF_BYTES {
        let cut = diff[..MAX_FILE_DIFF_BYTES]
            .rfind('\n')
            .unwrap_or(MAX_FILE_DIFF_BYTES);
        &diff[..cut]
    } else {
        diff
    };

    let messages = vec![
        Message::system(
            "Summarize this code change in one concise, technical sentence. \
             No filler like 'This change' or 'This commit'. \
             Output only the summary.",
        ),
        Message::user(format!("```diff\n{diff}\n```")),
    ];

    provider
        .complete(messages)
        .await
        .with_context(|| format!("failed to summarize {path}"))
}

/// Ask the model for only the subject line. The body is assembled by the
/// caller from the per-file summaries, so the model has a much simpler task.
async fn generate_subject(
    summaries: &[String],
    context: Option<&str>,
    format: CommitFormat,
    prompt_extra: Option<&str>,
    branch: Option<&str>,
    provider: &LlmProvider,
) -> Result<String> {
    let base = match format {
        CommitFormat::Conventional => "\
Write a single git commit subject line in conventional commit format: type(scope): description.
Rules: imperative mood, ≤72 characters, no period at the end.
Output only the subject line — nothing else, no explanation.",
        CommitFormat::Scoped => "\
Write a single git commit subject line as: <scope>: <description>.
Derive the scope from the changed paths (subsystem, package, tool name, path prefix — whatever best identifies the area).
Examples: \"git-commit: add dry-run flag\", \"net/http: fix redirect loop\", \"gitlab-ci: update image\"
Rules: imperative mood, ≤72 characters, no period at the end. No type prefix (feat/fix/etc).
Output only the subject line — nothing else, no explanation.",
    };
    let system = match prompt_extra {
        Some(extra) => format!("{base}\n{extra}"),
        None => base.to_string(),
    };

    let changes = summaries.join("\n");
    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = branch {
        parts.push(format!("Branch: {b}"));
    }
    if let Some(ctx) = context {
        parts.push(format!("Context: {ctx}"));
    }
    parts.push(format!("Changes:\n{changes}"));
    let user = parts.join("\n\n");

    let subject = provider
        .complete(vec![Message::system(system), Message::user(user)])
        .await
        .context("failed to generate commit subject")?;

    // Strip any stray newlines the model may have added
    Ok(subject.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string())
}

/// Preferred Ollama models, in order. The first one found on the running
/// Ollama instance is used when no model is explicitly configured.
/// Preferred Ollama models in descending order of quality.
/// gemma4 tags: "e4b" = 8B Q4_K_M, "e2b" = smaller efficient variant.
/// The 12b and larger models give the best results if you have the VRAM/RAM.
const OLLAMA_MODEL_PREFERENCE: &[&str] = &[
    "gemma4:12b",
    "gemma4:e4b",
    "gemma4:e2b",
    "gemma4:26b-a4b",
    "qwen3.5:2b",
    "qwen2.5:1.5b",
    "qwen3.5:0.8b",
];

async fn build_provider(cli: &Cli, cfg: &Config) -> Result<LlmProvider> {
    match cli.provider.to_lowercase().as_str() {
        "anthropic" | "claude" => {
            let api_key = cli
                .anthropic_api_key
                .clone()
                .context("ANTHROPIC_API_KEY is not set")?;
            // Priority: --model > --anthropic-model / ANTHROPIC_MODEL > config file > default
            let model = cli
                .model
                .as_deref()
                .or(cli.anthropic_model.as_deref())
                .or(cfg.anthropic_model.as_deref())
                .unwrap_or("claude-haiku-4-5")
                .to_string();
            Ok(LlmProvider::anthropic(api_key, model))
        }
        "ollama" => {
            // Priority: --ollama-url / OLLAMA_HOST > config file > default
            let url = cli
                .ollama_url
                .as_deref()
                .or(cfg.ollama_url.as_deref())
                .unwrap_or("http://localhost:11434")
                .to_string();
            // Priority: --model > --ollama-model / OLLAMA_MODEL > config file > auto-detect
            let model = if let Some(m) = cli
                .model
                .as_deref()
                .or(cli.ollama_model.as_deref())
                .or(cfg.ollama_model.as_deref())
            {
                m.to_string()
            } else {
                pick_ollama_model(&url).await
            };
            Ok(LlmProvider::ollama(url, model))
        }
        other => bail!("unknown provider '{other}' — expected 'anthropic' or 'ollama'"),
    }
}

/// Query the running Ollama instance and return the highest-preference model
/// that is available, falling back through `OLLAMA_MODEL_PREFERENCE` in order.
/// If Ollama is unreachable, returns the first entry in the preference list.
async fn pick_ollama_model(base_url: &str) -> String {
    match llm::list_ollama_models(base_url).await {
        Ok(available) => OLLAMA_MODEL_PREFERENCE
            .iter()
            .find(|&&pref| available.iter().any(|a| a.as_str() == pref))
            .map(|&s| s.to_string())
            .unwrap_or_else(|| {
                available
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| OLLAMA_MODEL_PREFERENCE[0].to_string())
            }),
        Err(_) => OLLAMA_MODEL_PREFERENCE[0].to_string(),
    }
}
