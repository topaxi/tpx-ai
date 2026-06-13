mod config;

use anyhow::{bail, Context, Result};
use clap::Parser;
use config::Config;
use llm::{LlmProvider, Message};
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
    let provider = build_provider(&cli, &cfg)?;
    let file_diffs = split_into_file_diffs(&diff);

    let message = if file_diffs.len() == 1 && file_diffs[0].1.len() <= 6_000 {
        // Single small file: one direct call
        let msgs = build_direct_messages(&file_diffs[0].1, cli.context.as_deref());
        provider.complete(msgs).await.context("LLM call failed")?
    } else {
        // Multiple files or large diff: summarize each file, then synthesize
        let summaries = summarize_all(&file_diffs, &provider).await?;
        generate_from_summaries(&summaries, cli.context.as_deref(), &provider).await?
    };

    let message = message.trim().to_string();

    if cli.dry_run {
        println!("{message}");
        return Ok(());
    }

    run_commit(&message)?;
    println!("✓ {message}");

    Ok(())
}

// ── git ──────────────────────────────────────────────────────────────────────

fn staged_diff() -> Result<String> {
    let out = Command::new("git")
        .args(["diff", "--staged"])
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

async fn generate_from_summaries(
    summaries: &[String],
    context: Option<&str>,
    provider: &LlmProvider,
) -> Result<String> {
    let system = "Write a single conventional git commit message for these changes. \
        Format: type(scope): description. \
        Use imperative mood. Keep under 72 characters. \
        Output only the commit message, nothing else.";

    let changes = summaries.join("\n");
    let user = match context {
        Some(ctx) => format!("Context: {ctx}\n\nChanges:\n{changes}"),
        None => format!("Changes:\n{changes}"),
    };

    provider
        .complete(vec![Message::system(system), Message::user(user)])
        .await
        .context("failed to generate commit message from summaries")
}

/// Single-file fast path: send the diff directly without a summary step.
fn build_direct_messages(diff: &str, context: Option<&str>) -> Vec<Message> {
    let system = "You are an expert at writing clear, concise git commit messages. \
        Follow the conventional commits specification: type(scope): description. \
        Use imperative mood. Keep the subject line under 72 characters. \
        Output only the commit message, nothing else.";

    let user = match context {
        Some(ctx) => format!("Context: {ctx}\n\nDiff:\n```diff\n{diff}\n```"),
        None => format!("Diff:\n```diff\n{diff}\n```"),
    };

    vec![Message::system(system), Message::user(user)]
}

fn build_provider(cli: &Cli, cfg: &Config) -> Result<LlmProvider> {
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
            // Priority: --model > --ollama-model / OLLAMA_MODEL > config file > default
            let model = cli
                .model
                .as_deref()
                .or(cli.ollama_model.as_deref())
                .or(cfg.ollama_model.as_deref())
                .unwrap_or("qwen3.5:0.8b")
                .to_string();
            // Priority: --ollama-url / OLLAMA_HOST > config file > default
            let url = cli
                .ollama_url
                .as_deref()
                .or(cfg.ollama_url.as_deref())
                .unwrap_or("http://localhost:11434");
            Ok(LlmProvider::ollama(url, model))
        }
        other => bail!("unknown provider '{other}' — expected 'anthropic' or 'ollama'"),
    }
}
