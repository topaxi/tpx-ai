use anyhow::{bail, Context, Result};
use clap::Parser;
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

    /// Model override (defaults: claude-haiku-4-5 for Anthropic, qwen2.5-coder for Ollama)
    #[arg(long, short = 'm')]
    model: Option<String>,

    /// Anthropic API key (overrides ANTHROPIC_API_KEY env var)
    #[arg(long, env = "ANTHROPIC_API_KEY")]
    anthropic_api_key: Option<String>,

    /// Ollama base URL
    #[arg(long, default_value = "http://localhost:11434", env = "OLLAMA_HOST")]
    ollama_url: String,

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

    let provider = build_provider(&cli)?;
    let messages = build_messages(&diff, cli.context.as_deref());

    let message = provider
        .complete(messages)
        .await
        .context("LLM call failed")?;

    let message = message.trim().to_string();

    if cli.dry_run {
        println!("{message}");
        return Ok(());
    }

    run_commit(&message)?;
    println!("✓ {message}");

    Ok(())
}

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

/// Split a unified diff into per-file sections and return only those whose
/// path matches at least one of `patterns` (glob or substring fallback).
fn filter_diff(diff: &str, patterns: &[String]) -> String {
    let mut result = String::new();
    let mut current = String::new();
    let mut include = false;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            // Flush the previous file block
            if include && !current.is_empty() {
                result.push_str(&current);
            }
            current.clear();

            // Extract the `a/<path>` portion (the `b/<path>` is after " b/")
            let path = rest.split_once(" b/").map_or(rest, |(p, _)| p);
            include = matches_any(path, patterns);
        }

        current.push_str(line);
        current.push('\n');
    }

    // Flush the last file block
    if include && !current.is_empty() {
        result.push_str(&current);
    }

    result
}

fn matches_any(path: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pat| {
        // Try glob first; fall back to substring
        glob::Pattern::new(pat)
            .map(|p| p.matches(path))
            .unwrap_or_else(|_| path.contains(pat.as_str()))
    })
}

fn build_messages(diff: &str, context: Option<&str>) -> Vec<Message> {
    let system = "\
You are an expert at writing concise, meaningful git commit messages. \
Follow the conventional commits specification: <type>(<scope>): <description>. \
Use imperative mood. Keep the subject line under 72 characters. \
Output only the commit message — no explanation, no markdown, no quotes.";

    let user = match context {
        Some(ctx) => format!("Context: {ctx}\n\nDiff:\n```\n{diff}\n```"),
        None => format!("Diff:\n```\n{diff}\n```"),
    };

    vec![Message::system(system), Message::user(user)]
}

fn build_provider(cli: &Cli) -> Result<LlmProvider> {
    match cli.provider.to_lowercase().as_str() {
        "anthropic" | "claude" => {
            let api_key = cli
                .anthropic_api_key
                .clone()
                .context("ANTHROPIC_API_KEY is not set (pass --anthropic-api-key or set the env var)")?;
            let model = cli
                .model
                .as_deref()
                .unwrap_or("claude-haiku-4-5")
                .to_string();
            Ok(LlmProvider::anthropic(api_key, model))
        }
        "ollama" => {
            let model = cli
                .model
                .as_deref()
                .unwrap_or("qwen2.5-coder")
                .to_string();
            Ok(LlmProvider::ollama(&cli.ollama_url, model))
        }
        other => bail!("unknown provider '{other}' — expected 'anthropic' or 'ollama'"),
    }
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
