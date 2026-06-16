mod config;

use anyhow::{bail, Context, Result};
use clap::Parser;
use config::{CommitFormat, Config};
use llm::{LlmProvider, Message};
use std::io::Write as _;
use std::path::Path;
use std::process::Command;

fn emit_progress(msg: &str, model: &str) {
    println!(
        "{}",
        serde_json::json!({"kind": "progress", "msg": msg, "model": model})
    );
    let _ = std::io::stdout().flush();
}

fn emit_body(text: &str) {
    println!("{}", serde_json::json!({"kind": "body", "text": text}));
    let _ = std::io::stdout().flush();
}

fn emit_subject(text: &str) {
    println!("{}", serde_json::json!({"kind": "subject", "text": text}));
    let _ = std::io::stdout().flush();
}

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

    /// LLM provider: anthropic or ollama [config: provider]
    /// Auto-detected when omitted: anthropic if ANTHROPIC_API_KEY is set, else ollama.
    #[arg(long)]
    provider: Option<String>,

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

    let all_diffs = split_into_file_diffs(&diff);
    if all_diffs.is_empty() {
        bail!("failed to parse any file diffs — this is a bug");
    }

    let excludes: Vec<&str> = DEFAULT_EXCLUDES
        .iter()
        .copied()
        .chain(cfg.commit_exclude.iter().map(String::as_str))
        .collect();

    let (file_diffs, skipped): (Vec<_>, Vec<_>) = all_diffs
        .into_iter()
        .partition(|(path, _)| !is_excluded(path, &excludes));

    if !skipped.is_empty() {
        let names: Vec<&str> = skipped.iter().map(|(p, _)| p.as_str()).collect();
        eprintln!("excluding {} file(s): {}", skipped.len(), names.join(", "));
    }

    if file_diffs.is_empty() {
        bail!("all changed files are excluded from diff analysis");
    }

    let file_paths: Vec<&str> = file_diffs.iter().map(|(p, _)| p.as_str()).collect();

    // Dry-run: emit NDJSON events to stdout so the Neovim plugin can parse them
    // as typed events rather than relying on line-prefix heuristics.
    if cli.dry_run {
        let model = provider.model_name().to_string();
        emit_progress(
            &format!("summarizing {} file(s)…", file_diffs.len()),
            &model,
        );
        let mut file_summaries = Vec::with_capacity(file_diffs.len());
        for (i, (path, content)) in file_diffs.iter().enumerate() {
            emit_progress(&format!("{}/{} {}…", i, file_diffs.len(), path), &model);
            let summary = summarize_file_diff(path, content, &provider)
                .await
                .with_context(|| format!("failed to summarize {path}"))?;
            emit_progress(
                &format!("{}/{} {}: {}", i + 1, file_diffs.len(), path, summary),
                &model,
            );
            file_summaries.push(format!("{path}: {summary}"));
        }
        emit_progress("consolidating changes…", &model);
        let bullets = consolidate_changes(&file_summaries, &provider).await?;
        for b in &bullets {
            emit_body(b);
        }
        emit_progress("generating subject…", &model);
        let subject = generate_subject(
            &bullets,
            &file_paths,
            cli.context.as_deref(),
            format,
            cfg.commit_prompt_extra.as_deref(),
            branch.as_deref(),
            &provider,
        )
        .await?;
        emit_subject(subject.trim());
        return Ok(());
    }

    let file_summaries = summarize_all(&file_diffs, &provider).await?;
    eprintln!("consolidating changes…");
    let bullets = consolidate_changes(&file_summaries, &provider).await?;
    let subject = generate_subject(
        &bullets,
        &file_paths,
        cli.context.as_deref(),
        format,
        cfg.commit_prompt_extra.as_deref(),
        branch.as_deref(),
        &provider,
    )
    .await?;
    let body = bullets
        .iter()
        .map(|b| format!("- {b}"))
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

// ── exclude list ─────────────────────────────────────────────────────────────

/// Files excluded from diff analysis by default. Patterns are matched against
/// both the full repo-relative path and the bare filename. Glob syntax (`*`,
/// `?`, `[...]`) is supported. Extend via `commit.exclude` in the config.
const DEFAULT_EXCLUDES: &[&str] = &[
    // Dependency lock files
    "package-lock.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "Cargo.lock",
    "Gemfile.lock",
    "poetry.lock",
    "Pipfile.lock",
    "uv.lock",
    "composer.lock",
    "packages.lock.json",
    "pubspec.lock",
    "go.sum",
    "mix.lock",
    "Podfile.lock",
    "flake.lock",
    "gradle.lockfile",
    ".terraform.lock.hcl",
    // Minified / bundled assets
    "*.min.js",
    "*.min.css",
    // Source maps
    "*.map",
    // Generated protobuf
    "*.pb.go",
    "*.pb.ts",
    "*_pb.ts",
    "*_pb.js",
];

fn is_excluded(path: &str, patterns: &[&str]) -> bool {
    let basename = Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    patterns.iter().any(|&pat| {
        glob::Pattern::new(pat)
            .map(|p| p.matches(basename) || p.matches(path))
            .unwrap_or_else(|_| basename == pat || path.contains(pat))
    })
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

/// Truncate a diff to `limit` bytes, preferring a clean hunk boundary (`\n@@`).
/// Falls back to the last newline within the limit when no prior complete hunk exists.
fn truncate_diff(diff: &str, limit: usize) -> &str {
    if diff.len() <= limit {
        return diff;
    }
    let within = &diff[..limit];
    // Find the last hunk header inside the limit and truncate before it,
    // but only when at least one earlier hunk exists (so we keep some content).
    if let Some(pos) = within.rfind("\n@@") {
        if within[..pos].contains("\n@@") {
            return &diff[..pos];
        }
    }
    // Fall back: cut at the last newline.
    let cut = within.rfind('\n').unwrap_or(limit);
    &diff[..cut]
}

async fn summarize_file_diff(path: &str, diff: &str, provider: &LlmProvider) -> Result<String> {
    let diff = truncate_diff(diff, MAX_FILE_DIFF_BYTES);

    let messages = vec![
        Message::system(
            "Summarize what this code change does in one concise sentence. \
             Focus on what behaviour or functionality changes, not which variables or functions were modified. \
             No filler like 'This change' or 'This commit'. \
             Output only the summary.",
        ),
        Message::user(format!("File: {path}\n\n```diff\n{diff}\n```")),
    ];

    provider
        .complete(messages)
        .await
        .with_context(|| format!("failed to summarize {path}"))
}

/// Collapse per-file summaries into a short bullet list of conceptual changes.
/// For a single file the LLM call is skipped — the summary is used directly.
async fn consolidate_changes(
    file_summaries: &[String],
    provider: &LlmProvider,
) -> Result<Vec<String>> {
    if file_summaries.len() == 1 {
        let text = file_summaries[0]
            .split_once(": ")
            .map(|(_, s)| s.to_string())
            .unwrap_or_else(|| file_summaries[0].clone());
        return Ok(vec![text]);
    }

    let input = file_summaries.join("\n");
    let output = provider
        .complete(vec![
            Message::system(
                "You are given per-file change summaries from a git diff.\n\
                 Synthesize them into a concise bullet list of what this commit achieves: \
                 new behaviour, features, fixes, or capabilities introduced.\n\
                 Group related changes into single bullets. \
                 Focus on the developer-visible outcome, not which files or functions were modified.\n\
                 Imperative mood. One bullet per logical change. Prefix each with \"- \".\n\
                 Output only the bullet list, nothing else.",
            ),
            Message::user(input),
        ])
        .await
        .context("failed to consolidate changes")?;

    let bullets: Vec<String> = output
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            l.strip_prefix("- ")
                .or_else(|| l.strip_prefix("* "))
                .map(|s| s.to_string())
        })
        .collect();

    if bullets.is_empty() {
        // Fallback: strip file paths from the raw summaries
        Ok(file_summaries
            .iter()
            .map(|s| {
                s.split_once(": ")
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_else(|| s.clone())
            })
            .collect())
    } else {
        Ok(bullets)
    }
}

/// Generate the subject line from consolidated bullets.
async fn generate_subject(
    bullets: &[String],
    file_paths: &[&str],
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
Infer the scope from the affected files and the nature of the changes (subsystem, tool, component, or module name).
Examples: \"git-commit: add dry-run flag\", \"net/http: fix redirect loop\", \"gitlab-ci: update image\"
Rules: imperative mood, ≤72 characters, no period at the end. No type prefix (feat/fix/etc).
Output only the subject line — nothing else, no explanation.",
    };
    let system = match prompt_extra {
        Some(extra) => format!("{base}\n{extra}"),
        None => base.to_string(),
    };

    let mut parts: Vec<String> = Vec::new();
    if let Some(b) = branch {
        parts.push(format!("Branch: {b}"));
    }
    if let Some(ctx) = context {
        parts.push(format!("Context: {ctx}"));
    }
    if !file_paths.is_empty() {
        parts.push(format!("Affected files:\n{}", file_paths.join("\n")));
    }
    parts.push(format!(
        "Changes:\n{}",
        bullets
            .iter()
            .map(|b| format!("- {b}"))
            .collect::<Vec<_>>()
            .join("\n")
    ));
    let user = parts.join("\n\n");

    let subject = provider
        .complete(vec![Message::system(system), Message::user(user)])
        .await
        .context("failed to generate commit subject")?;

    Ok(subject
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string())
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

fn auto_detect_provider() -> &'static str {
    if std::env::var("ANTHROPIC_API_KEY")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        "anthropic"
    } else {
        "ollama"
    }
}

async fn build_provider(cli: &Cli, cfg: &Config) -> Result<LlmProvider> {
    let provider = cli
        .provider
        .as_deref()
        .or(cfg.provider.as_deref())
        .map(|s| s.to_lowercase())
        .unwrap_or_else(|| auto_detect_provider().to_string());

    match provider.as_str() {
        "anthropic" | "claude" => {
            let api_key = cli
                .anthropic_api_key
                .clone()
                .context("ANTHROPIC_API_KEY is not set")?;
            // Priority: --model > --anthropic-model / ANTHROPIC_MODEL > config file > default
            let model = if let Some(m) = cli.model.as_deref().or(cli.anthropic_model.as_deref()) {
                m.to_string()
            } else if let Some(models) = &cfg.anthropic_model {
                if models.len() > 1 {
                    bail!(
                        "model lists are not supported for Anthropic — Anthropic does not expose \
                         a model listing API; specify a single model name in config"
                    );
                }
                models[0].clone()
            } else {
                "claude-haiku-4-5".to_string()
            };
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
            let model = if let Some(m) = cli.model.as_deref().or(cli.ollama_model.as_deref()) {
                m.to_string()
            } else if let Some(models) = &cfg.ollama_model {
                pick_model_from_list(models, &url).await?
            } else {
                pick_ollama_model(&url).await
            };
            Ok(LlmProvider::ollama(url, model))
        }
        other => bail!("unknown provider '{other}' — expected 'anthropic' or 'ollama'"),
    }
}

/// Given an ordered preference list from config, query Ollama and return the first
/// configured model that is actually installed. Errors if none are available.
async fn pick_model_from_list(models: &[String], base_url: &str) -> Result<String> {
    let available = llm::list_ollama_models(base_url)
        .await
        .with_context(|| format!("failed to query Ollama models at {base_url}"))?;
    models
        .iter()
        .find(|m| available.iter().any(|a| a == *m))
        .cloned()
        .with_context(|| {
            format!(
                "none of the configured models ({}) are available on Ollama at {base_url}",
                models.join(", ")
            )
        })
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
