use std::fmt::Write as FmtWrite;
use std::io::{self, BufWriter, Read, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

// ── ANSI colours ─────────────────────────────────────────────────────────────

const BLUE: &str = "\x1b[38;2;97;175;239m";
const AMBER: &str = "\x1b[38;2;229;192;123m";
const CYAN: &str = "\x1b[38;2;86;182;194m";
const GREEN: &str = "\x1b[38;2;80;200;120m";
const ORANGE: &str = "\x1b[38;2;255;176;85m";
const YELLOW: &str = "\x1b[38;2;230;200;0m";
const RED: &str = "\x1b[38;2;235;87;87m";
const MAGENTA: &str = "\x1b[38;2;198;120;221m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";
const SEP: &str = " \x1b[2m│\x1b[0m ";

// ── JSON input types ──────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct StatusInput {
    model: Option<ModelInfo>,
    workspace: Option<WorkspaceInfo>,
    cwd: Option<String>,
    context_window: Option<ContextWindow>,
    rate_limits: Option<RateLimits>,
    cost: Option<CostInfo>,
    session_id: Option<String>,
    session_name: Option<String>,
    effort: Option<EffortInfo>,
    agent: Option<AgentInfo>,
}

#[derive(Deserialize)]
struct ModelInfo {
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct WorkspaceInfo {
    current_dir: Option<String>,
}

#[derive(Deserialize)]
struct ContextWindow {
    used_percentage: Option<f64>,
    total_input_tokens: Option<u64>,
    total_output_tokens: Option<u64>,
    current_usage: Option<CurrentUsage>,
}

#[derive(Deserialize)]
struct CurrentUsage {
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct RateLimits {
    five_hour: Option<RateLimit>,
    seven_day: Option<RateLimit>,
}

#[derive(Deserialize)]
struct RateLimit {
    used_percentage: Option<f64>,
    resets_at: Option<u64>,
}

#[derive(Deserialize)]
struct CostInfo {
    total_cost_usd: Option<f64>,
    total_api_duration_ms: Option<u64>,
    total_lines_added: Option<i64>,
    total_lines_removed: Option<i64>,
}

#[derive(Deserialize)]
struct EffortInfo {
    level: Option<String>,
}

#[derive(Deserialize)]
struct AgentInfo {
    name: Option<String>,
}

// ── Config ────────────────────────────────────────────────────────────────────

struct Config {
    budget: Option<f64>,
    budget_raw: Option<String>,
    initial_usage: Option<f64>,
    start_ts: Option<u64>,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn fmt_time(m: u64) -> String {
    if m > 99 {
        format!("{}h", m / 60)
    } else {
        format!("{}m", m)
    }
}

/// Returns an ANSI-coloured pace arrow string, or empty if nothing to show.
///
/// projected% = used% × duration / elapsed.
/// ↑ will exhaust before reset | → on pace | ↓ under-consuming
fn pace_arrow(used_pct: f64, resets_at: Option<u64>, duration: u64, now: u64) -> String {
    let resets_at = match resets_at {
        Some(r) => r,
        None => return String::new(),
    };

    let period_start = resets_at.saturating_sub(duration);
    let elapsed = now.saturating_sub(period_start);
    if elapsed == 0 || elapsed <= duration / 50 {
        return String::new();
    }

    let projected = used_pct * duration as f64 / elapsed as f64;

    let remaining_m = resets_at.saturating_sub(now) / 60;
    let time_left_m: Option<u64> = if used_pct > 0.0 {
        let tlm = (100.0 - used_pct) * elapsed as f64 / used_pct / 60.0;
        if tlm >= 0.0 { Some(tlm as u64) } else { None }
    } else {
        None
    };

    let time_color = match time_left_m {
        Some(tlm) if remaining_m > 0 => {
            let ratio = tlm * 100 / remaining_m;
            if ratio < 33 { RED } else if ratio < 66 { ORANGE } else { GREEN }
        }
        _ => GREEN,
    };

    let mut out = String::with_capacity(48);

    if projected > 115.0 {
        write!(out, "{}↑{}", RED, RESET).unwrap();
        if let Some(tlm) = time_left_m {
            write!(out, " {}{}{}", time_color, fmt_time(tlm), RESET).unwrap();
        }
    } else if projected > 85.0 {
        write!(out, "{}→{}", YELLOW, RESET).unwrap();
        if let Some(tlm) = time_left_m {
            write!(out, " {}{}{}", time_color, fmt_time(tlm), RESET).unwrap();
        }
    } else {
        // ↓ = under-consuming; don't show time_left
        write!(out, "{}↓{}", GREEN, RESET).unwrap();
    }

    out
}

/// Builds a separator-joined status line segment by segment.
struct LineBuilder {
    buf: String,
    empty: bool,
}

impl LineBuilder {
    fn new() -> Self {
        Self {
            buf: String::with_capacity(512),
            empty: true,
        }
    }

    fn add(&mut self, segment: &str) {
        if segment.is_empty() {
            return;
        }
        if !self.empty {
            self.buf.push_str(SEP);
        }
        self.buf.push_str(segment);
        self.empty = false;
    }

    fn into_string(self) -> String {
        self.buf
    }
}

// ── Paths ─────────────────────────────────────────────────────────────────────

fn get_usage_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_USAGE_DIR") {
        return PathBuf::from(dir);
    }
    get_claude_config_dir().join("usage")
}

fn get_claude_config_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude")
        })
}

// ── Config file ───────────────────────────────────────────────────────────────

fn load_config(usage_dir: &Path) -> Config {
    let content = match std::fs::read_to_string(usage_dir.join(".config")) {
        Ok(c) => c,
        Err(_) => {
            return Config {
                budget: None,
                budget_raw: None,
                initial_usage: None,
                start_ts: None,
            }
        }
    };

    let mut budget: Option<f64> = None;
    let mut budget_raw: Option<String> = None;
    let mut initial_usage: Option<f64> = None;
    let mut start_ts: Option<u64> = None;

    for line in content.lines() {
        if let Some(v) = line.strip_prefix("budget=") {
            budget = v.parse().ok();
            budget_raw = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("initial_usage=") {
            initial_usage = v.parse().ok();
        } else if let Some(v) = line.strip_prefix("start_ts=") {
            start_ts = v.parse().ok();
        }
    }

    Config {
        budget,
        budget_raw,
        initial_usage,
        start_ts,
    }
}

fn update_config(usage_dir: &Path, updates: &[(&str, &str)]) -> anyhow::Result<()> {
    let config_path = usage_dir.join(".config");
    if !config_path.exists() {
        anyhow::bail!("no config at {}", config_path.display());
    }

    let content = std::fs::read_to_string(&config_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut found = vec![false; updates.len()];

    for line in &mut lines {
        for (i, &(key, val)) in updates.iter().enumerate() {
            if line.starts_with(&format!("{key}=")) {
                *line = format!("{key}={val}");
                found[i] = true;
            }
        }
    }

    for (i, &(key, val)) in updates.iter().enumerate() {
        if !found[i] {
            lines.push(format!("{key}={val}"));
        }
    }

    std::fs::write(&config_path, lines.join("\n") + "\n")?;
    Ok(())
}

fn cat_config(usage_dir: &Path) -> anyhow::Result<()> {
    print!("{}", std::fs::read_to_string(usage_dir.join(".config"))?);
    Ok(())
}

// ── Usage tracking ────────────────────────────────────────────────────────────

/// Sum all session file costs (including offset files with negative values)
/// where the first-column timestamp >= start_ts.
fn calculate_period_total(usage_dir: &Path, start_ts: u64, initial_usage: f64) -> Option<f64> {
    let mut tracked = 0.0f64;
    for entry in std::fs::read_dir(usage_dir).ok()?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(line) = content.lines().next() else {
            continue;
        };
        let mut fields = line.splitn(3, '\t');
        let ts: u64 = fields.next().unwrap_or("").parse().unwrap_or(0);
        let cost: f64 = fields.next().unwrap_or("").parse().unwrap_or(0.0);
        if ts >= start_ts {
            tracked += cost;
        }
    }
    Some(initial_usage + tracked)
}

// ── sync subcommand ───────────────────────────────────────────────────────────

fn sync_sessions(usage_dir: &Path, real_usage: &str) -> anyhow::Result<()> {
    let now = now_unix().to_string();

    for entry in std::fs::read_dir(usage_dir)?.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.ends_with("_offset") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(line) = content.lines().next() else {
            continue;
        };
        let cost = line.split('\t').nth(1).unwrap_or("").trim();
        if cost.is_empty() {
            continue;
        }
        let offset_path = path.with_file_name(format!("{name}_offset"));
        let _ = std::fs::write(
            offset_path,
            format!("{now}\t-{cost}\toffset\t0\t0\t0\n"),
        );
    }

    update_config(usage_dir, &[("initial_usage", real_usage), ("start_ts", "0")])
}

// ── git helpers ───────────────────────────────────────────────────────────────

fn git_branch(cwd: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", cwd, "--no-optional-locks", "symbolic-ref", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = std::str::from_utf8(&out.stdout).ok()?.trim();
    if s.is_empty() { None } else { Some(s.to_string()) }
}

struct GitStatus {
    clean: bool,
    staged: usize,
    modified: usize,
    untracked: usize,
}

fn git_status(cwd: &str) -> Option<GitStatus> {
    let out = Command::new("git")
        .args(["-C", cwd, "--no-optional-locks", "status", "--porcelain"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let porcelain = std::str::from_utf8(&out.stdout).ok()?;
    if porcelain.is_empty() {
        return Some(GitStatus { clean: true, staged: 0, modified: 0, untracked: 0 });
    }

    let (mut staged, mut modified, mut untracked) = (0usize, 0usize, 0usize);
    for line in porcelain.lines() {
        let b = line.as_bytes();
        if b.len() < 2 {
            continue;
        }
        let (x, y) = (b[0] as char, b[1] as char);
        if matches!(x, 'M' | 'A' | 'D' | 'R' | 'C') {
            staged += 1;
        }
        if matches!(y, 'M' | 'D') {
            modified += 1;
        }
        if x == '?' && y == '?' {
            untracked += 1;
        }
    }
    Some(GitStatus { clean: false, staged, modified, untracked })
}

// ── statusline ────────────────────────────────────────────────────────────────

fn run_statusline() -> anyhow::Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    if input.trim().is_empty() {
        print!("Claude");
        return Ok(());
    }

    let status: StatusInput = match serde_json::from_str(&input) {
        Ok(s) => s,
        Err(_) => {
            print!("Claude");
            return Ok(());
        }
    };

    let now = now_unix();
    let usage_dir = get_usage_dir();

    // ── Extract fields ────────────────────────────────────────────────────────
    let model_raw = status.model.as_ref().and_then(|m| m.display_name.as_deref());

    // "Claude Opus 4.6 (1M context)" → "Opus 4.6 (1M)"
    let model_display: Option<String> = model_raw.map(|m| {
        m.strip_prefix("Claude ").unwrap_or(m).replace(" context", "")
    });

    let cwd = status
        .workspace
        .as_ref()
        .and_then(|w| w.current_dir.as_deref())
        .or(status.cwd.as_deref());

    let used_pct = status.context_window.as_ref().and_then(|c| c.used_percentage);

    let rl_five = status.rate_limits.as_ref().and_then(|r| r.five_hour.as_ref());
    let rl_five_pct = rl_five.and_then(|r| r.used_percentage);
    let rl_five_resets = rl_five.and_then(|r| r.resets_at);

    let rl_seven = status.rate_limits.as_ref().and_then(|r| r.seven_day.as_ref());
    let rl_seven_pct = rl_seven.and_then(|r| r.used_percentage);
    let rl_seven_resets = rl_seven.and_then(|r| r.resets_at);

    let cost_usd = status.cost.as_ref().and_then(|c| c.total_cost_usd);
    let api_duration_ms = status.cost.as_ref().and_then(|c| c.total_api_duration_ms);
    let lines_added = status.cost.as_ref().and_then(|c| c.total_lines_added).unwrap_or(0);
    let lines_removed = status.cost.as_ref().and_then(|c| c.total_lines_removed).unwrap_or(0);

    let total_input = status
        .context_window
        .as_ref()
        .and_then(|c| c.total_input_tokens)
        .unwrap_or(0);
    let total_output = status
        .context_window
        .as_ref()
        .and_then(|c| c.total_output_tokens)
        .unwrap_or(0);
    let cache_read = status
        .context_window
        .as_ref()
        .and_then(|c| c.current_usage.as_ref())
        .and_then(|u| u.cache_read_input_tokens)
        .unwrap_or(0);
    let cache_create = status
        .context_window
        .as_ref()
        .and_then(|c| c.current_usage.as_ref())
        .and_then(|u| u.cache_creation_input_tokens)
        .unwrap_or(0);

    let session_id = status.session_id.as_deref();
    let session_name = status.session_name.as_deref();
    let effort_level = status.effort.as_ref().and_then(|e| e.level.as_deref());
    let agent_name = status.agent.as_ref().and_then(|a| a.name.as_deref());

    // ── Git ───────────────────────────────────────────────────────────────────
    let branch = cwd.and_then(git_branch);
    let git_st = cwd.and(branch.as_deref()).and_then(|_| cwd.and_then(git_status));

    // ── Write session cost file ───────────────────────────────────────────────
    if let (Some(sid), Some(cost)) = (session_id, cost_usd) {
        if rl_five_pct.is_none() && rl_seven_pct.is_none() {
            let _ = std::fs::create_dir_all(&usage_dir);
            let _ = std::fs::write(
                usage_dir.join(sid),
                format!(
                    "{}\t{}\t{}\t{}\t{}\t{}\n",
                    now,
                    cost,
                    model_raw.unwrap_or("unknown"),
                    api_duration_ms.unwrap_or(0),
                    lines_added,
                    lines_removed,
                ),
            );
        }
    }

    // ── Load config + period total ────────────────────────────────────────────
    let config = load_config(&usage_dir);
    let period_total = match (config.budget, config.start_ts) {
        (Some(_), Some(ts)) => {
            calculate_period_total(&usage_dir, ts, config.initial_usage.unwrap_or(0.0))
        }
        _ => None,
    };

    // ── Wakeup file ───────────────────────────────────────────────────────────
    let wakeup_file = get_claude_config_dir().join("next-wakeup");
    let mut wakeup_fmt: Option<String> = None;
    let mut wakeup_reason: Option<String> = None;

    if let Ok(content) = std::fs::read_to_string(&wakeup_file) {
        if let Some(first) = content.lines().next() {
            let (ts_str, reason) = first
                .split_once('\t')
                .map(|(a, b)| (a, Some(b)))
                .unwrap_or((first, None));

            if let Ok(wakeup_ts) = ts_str.parse::<u64>() {
                if wakeup_ts > now {
                    let left = wakeup_ts - now;
                    wakeup_fmt = Some(if left < 60 {
                        format!("{left}s")
                    } else if left < 3600 {
                        format!("{}m{}s", left / 60, left % 60)
                    } else {
                        format!("{}h{}m", left / 3600, (left % 3600) / 60)
                    });
                    wakeup_reason = reason.map(|r| r.to_string());
                } else {
                    let _ = std::fs::remove_file(&wakeup_file);
                }
            }
        }
    }

    // ── Assemble line 1 ───────────────────────────────────────────────────────
    let stdout = io::stdout();
    let mut out = BufWriter::new(stdout.lock());
    let mut line1 = LineBuilder::new();
    let mut seg = String::with_capacity(128);

    // Model — amber for Opus, cyan for Haiku, blue otherwise
    if let Some(model) = &model_display {
        let color = if model.contains("Opus") {
            AMBER
        } else if model.contains("Haiku") {
            CYAN
        } else {
            BLUE
        };
        seg.clear();
        write!(seg, "{color}{model}{RESET}").unwrap();
        line1.add(&seg);
    }

    // Effort level
    if let Some(level) = effort_level {
        let color = match level {
            "low" => DIM,
            "medium" => CYAN,
            "high" => ORANGE,
            "xhigh" | "max" => RED,
            _ => DIM,
        };
        seg.clear();
        write!(seg, "{color}{level}{RESET}").unwrap();
        line1.add(&seg);
    }

    // Agent name
    if let Some(name) = agent_name {
        seg.clear();
        write!(seg, "{DIM}agent:{RESET}{CYAN}{name}{RESET}").unwrap();
        line1.add(&seg);
    }

    // Git branch + dirty state
    if let Some(br) = &branch {
        seg.clear();
        write!(seg, "{DIM}⎇{RESET} {MAGENTA}{br}{RESET}").unwrap();
        match &git_st {
            Some(st) if st.clean => write!(seg, " {GREEN}{DIM}✓{RESET}").unwrap(),
            Some(st) => {
                if st.staged > 0 {
                    write!(seg, " {GREEN}+{}{RESET}", st.staged).unwrap();
                }
                if st.modified > 0 {
                    write!(seg, " {YELLOW}~{}{RESET}", st.modified).unwrap();
                }
                if st.untracked > 0 {
                    write!(seg, " {DIM}?{}{RESET}", st.untracked).unwrap();
                }
            }
            None => {}
        }
        line1.add(&seg);
    }

    // Pending ScheduleWakeup
    if let Some(ref wfmt) = wakeup_fmt {
        seg.clear();
        write!(seg, "{YELLOW}⏰ {wfmt}{RESET}").unwrap();
        line1.add(&seg);
    }

    // Context window %
    if let Some(used) = used_pct {
        let pct = used.round() as i64;
        let color = if pct >= 80 { RED } else if pct >= 50 { ORANGE } else { CYAN };
        seg.clear();
        write!(seg, "{DIM}ctx{RESET} {color}{pct}%{RESET}").unwrap();
        line1.add(&seg);
    }

    // Rate limits — 5h
    if let Some(f_pct) = rl_five_pct {
        let f = f_pct.round() as i64;
        let color = if f >= 80 { RED } else if f >= 50 { YELLOW } else { CYAN };
        let arrow = pace_arrow(f_pct, rl_five_resets, 18000, now);
        seg.clear();
        match rl_five_resets.filter(|&r| r > now) {
            Some(resets_at) => {
                let t = fmt_time((resets_at - now) / 60);
                write!(seg, "{color}{t}:{f}%{arrow}{RESET}").unwrap();
            }
            None => write!(seg, "{color}{f}%{arrow}{RESET}").unwrap(),
        }
        line1.add(&seg);
    }

    // Rate limits — 7d
    if let Some(s_pct) = rl_seven_pct {
        let s = s_pct.round() as i64;
        let arrow = pace_arrow(s_pct, rl_seven_resets, 604800, now);
        seg.clear();
        match rl_seven_resets.filter(|&r| r > now) {
            Some(resets_at) => {
                let t = fmt_time((resets_at - now) / 60);
                write!(seg, "{CYAN}{t}:{s}%{arrow}{RESET}").unwrap();
            }
            None => write!(seg, "{CYAN}7d:{s}%{arrow}{RESET}").unwrap(),
        }
        line1.add(&seg);
    }

    // Cache hit rate — shown alongside rate limits
    if rl_five_pct.is_some() || rl_seven_pct.is_some() {
        if let Some(hit) = (cache_read * 100).checked_div(cache_read + cache_create) {
            let color = if hit >= 80 { GREEN } else if hit >= 50 { CYAN } else { ORANGE };
            seg.clear();
            write!(seg, "{DIM}cache{RESET} {color}{hit}%{RESET}").unwrap();
            line1.add(&seg);
        }
    }

    // Cost section — shown when no rate limits (API/enterprise billing)
    if rl_five_pct.is_none() && rl_seven_pct.is_none() {
        if let Some(cost) = cost_usd {
            let active_rate: Option<f64> = api_duration_ms
                .filter(|&ms| ms > 0)
                .map(|ms| cost / (ms as f64 / 3_600_000.0));

            // Build cost string with optional burn rate
            let mut cost_seg = String::with_capacity(64);
            write!(cost_seg, "{GREEN}${cost:.2}{RESET}").unwrap();
            if let Some(rate) = active_rate {
                write!(cost_seg, " {DIM}{rate:.2}/hr{RESET}").unwrap();
            }

            // Cache hit rate — combine with cost into one add() call
            match (cache_read * 100).checked_div(cache_read + cache_create) {
                Some(hit) => {
                    let color = if hit >= 80 { GREEN } else if hit >= 50 { CYAN } else { ORANGE };
                    seg.clear();
                    write!(seg, "{cost_seg}{SEP}{DIM}cache{RESET} {color}{hit}%{RESET}").unwrap();
                    line1.add(&seg);
                }
                None => line1.add(&cost_seg),
            }

            // Cost per 1k tokens + net lines
            let total_tokens = total_input + total_output;
            if total_tokens > 0 {
                let cpk = cost * 1000.0 / total_tokens as f64;
                let net = lines_added - lines_removed;
                let (net_color, net_sign) = if net >= 0 { (GREEN, "+") } else { (RED, "") };
                seg.clear();
                write!(seg, "{DIM}${cpk:.2}/kt{RESET} {net_color}{net_sign}{net}{RESET}").unwrap();
                line1.add(&seg);
            }

            // Budget tracking — accumulated across billing period
            if let (Some(pt), Some(budget), Some(rate)) =
                (period_total, config.budget, active_rate)
            {
                if budget > 0.0 {
                    let budget_pct = (pt * 100.0 / budget) as i64;
                    let bgt_color =
                        if budget_pct >= 80 { RED } else if budget_pct >= 50 { YELLOW } else { CYAN };

                    let remaining = budget - pt;
                    let time_left_m: Option<i64> = (rate > 0.0)
                        .then(|| (remaining * 60.0 / rate) as i64);

                    let budget_raw = config.budget_raw.as_deref().unwrap_or("");
                    seg.clear();
                    write!(seg, "{bgt_color}${pt:.2}/{budget_raw}").unwrap();
                    if let Some(tlm) = time_left_m.filter(|&t| t >= 0) {
                        write!(seg, " {DIM}{}{RESET}", fmt_time(tlm as u64)).unwrap();
                    }
                    write!(seg, "{RESET}").unwrap();
                    line1.add(&seg);
                }
            }
        }
    }

    out.write_all(line1.into_string().as_bytes())?;
    out.write_all(b"\n")?;

    // ── Line 2: cwd + session name + wakeup reason ────────────────────────────
    let mut line2 = LineBuilder::new();

    if let Some(c) = cwd {
        let home = std::env::var("HOME").unwrap_or_default();
        let display = if !home.is_empty() && c.starts_with(&home) {
            format!("~{}", &c[home.len()..])
        } else {
            c.to_string()
        };
        seg.clear();
        write!(seg, "{DIM}{display}{RESET}").unwrap();
        line2.add(&seg);
    }

    if let Some(name) = session_name {
        seg.clear();
        write!(seg, "{DIM}{name}{RESET}").unwrap();
        line2.add(&seg);
    }

    if let Some(ref reason) = wakeup_reason {
        seg.clear();
        write!(seg, "{DIM}⏰ {reason}{RESET}").unwrap();
        line2.add(&seg);
    }

    let line2_str = line2.into_string();
    if !line2_str.is_empty() {
        out.write_all(line2_str.as_bytes())?;
        out.write_all(b"\n")?;
    }

    out.flush()?;
    Ok(())
}

// ── main ──────────────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let usage_dir = get_usage_dir();

    match args.get(1).map(String::as_str) {
        Some("budget") => {
            let val = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("budget: missing value"))?;
            update_config(&usage_dir, &[("budget", val)])?;
            cat_config(&usage_dir)?;
        }
        Some("usage") => {
            let val = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: missing value"))?;
            update_config(&usage_dir, &[("initial_usage", val)])?;
            cat_config(&usage_dir)?;
        }
        Some("sync") => {
            let val = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("sync: missing value"))?;
            sync_sessions(&usage_dir, val)?;
            cat_config(&usage_dir)?;
        }
        _ => run_statusline()?,
    }

    Ok(())
}
