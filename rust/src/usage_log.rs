use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

const LOG_FILE_NAME: &str = "usage-log.jsonl";
const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageLogSample {
    pub timestamp: DateTime<Utc>,
    pub provider: String,
    pub account: Option<String>,
    pub source_label: Option<String>,
    pub plan: Option<String>,
    pub model_name: Option<String>,
    pub session_percent: Option<f64>,
    pub weekly_percent: Option<f64>,
    pub model_percent: Option<f64>,
    pub codex_tokens: Option<CodexTokenLogSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTokenLogSample {
    pub last_model: Option<String>,
    pub input_tokens_total: i64,
    pub cached_tokens_total: i64,
    pub output_tokens_total: i64,
    pub total_tokens_total: i64,
    pub input_tokens_delta: i64,
    pub cached_tokens_delta: i64,
    pub output_tokens_delta: i64,
    pub total_tokens_delta: i64,
    pub by_model_total: HashMap<String, CodexTokenModelLogTotals>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexTokenModelLogTotals {
    pub input_tokens: i64,
    pub cached_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Copy)]
pub enum UsageTrendComparison {
    Faster,
    Slower,
    Steady,
    ResetOrDrop,
    Insufficient,
}

#[derive(Debug, Clone)]
pub struct UsageTrendSummary {
    pub label: &'static str,
    pub delta_percent: f64,
    pub per_hour: f64,
    pub average_per_day_24h: Option<f64>,
    pub average_per_day_7d: Option<f64>,
    pub comparison: UsageTrendComparison,
    pub sample_count: usize,
}

impl UsageLogSample {
    pub fn has_usage(&self) -> bool {
        self.session_percent.is_some()
            || self.weekly_percent.is_some()
            || self.model_percent.is_some()
    }
}

pub fn usage_log_path() -> Option<PathBuf> {
    usage_data_dir().map(|path| path.join(LOG_FILE_NAME))
}

fn usage_data_dir() -> Option<PathBuf> {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(repo_root) = exe_path.ancestors().find(|path| {
            path.join(".git").exists() && path.join("rust").join("Cargo.toml").exists()
        }) {
            return Some(repo_root.join("data"));
        }

        if let Some(exe_dir) = exe_path.parent() {
            return Some(exe_dir.join("data"));
        }
    }

    dirs::config_dir().map(|path| path.join("CodexBar").join("data"))
}

pub fn append_and_summarize_usage(sample: UsageLogSample) -> Vec<UsageTrendSummary> {
    let mut sample = sample;
    hydrate_codex_token_delta(&mut sample);

    if sample.has_usage() {
        if let Err(err) = append_usage_sample(&sample) {
            tracing::warn!("Failed to append usage log sample: {}", err);
        }
    }

    summarize_usage(&sample)
}

fn hydrate_codex_token_delta(sample: &mut UsageLogSample) {
    let Some(previous) = load_relevant_entries(sample)
        .into_iter()
        .rev()
        .find_map(|entry| entry.codex_tokens)
    else {
        return;
    };
    let Some(current) = sample.codex_tokens.as_mut() else {
        return;
    };

    current.input_tokens_delta =
        (current.input_tokens_total - previous.input_tokens_total).max(0);
    current.cached_tokens_delta =
        (current.cached_tokens_total - previous.cached_tokens_total).max(0);
    current.output_tokens_delta =
        (current.output_tokens_total - previous.output_tokens_total).max(0);
    current.total_tokens_delta =
        (current.total_tokens_total - previous.total_tokens_total).max(0);
}

fn append_usage_sample(sample: &UsageLogSample) -> anyhow::Result<()> {
    let path = usage_log_path().ok_or_else(|| anyhow::anyhow!("Could not determine log path"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string(sample)?;
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(file, "{}", json)?;
    Ok(())
}

fn summarize_usage(sample: &UsageLogSample) -> Vec<UsageTrendSummary> {
    let mut entries = load_relevant_entries(sample);
    entries.push(sample.clone());
    entries.sort_by_key(|entry| entry.timestamp);

    let mut summaries = Vec::new();
    add_summary(&mut summaries, &entries, "Primary", |entry| {
        entry.session_percent
    });
    add_summary(&mut summaries, &entries, "Secondary", |entry| {
        entry.weekly_percent
    });
    add_summary(&mut summaries, &entries, "Model", |entry| entry.model_percent);
    summaries
}

fn load_relevant_entries(sample: &UsageLogSample) -> Vec<UsageLogSample> {
    let Some(path) = usage_log_path() else {
        return Vec::new();
    };
    let Ok(metadata) = fs::metadata(&path) else {
        return Vec::new();
    };
    if metadata.len() > MAX_READ_BYTES {
        tracing::warn!(
            "Usage log is larger than {} bytes; skipping historical summary",
            MAX_READ_BYTES
        );
        return Vec::new();
    }

    let Ok(content) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let cutoff = sample.timestamp - Duration::days(30);
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<UsageLogSample>(line).ok())
        .filter(|entry| {
            entry.provider == sample.provider
                && entry.account == sample.account
                && entry.timestamp >= cutoff
                && entry.timestamp <= sample.timestamp
        })
        .collect()
}

fn add_summary(
    summaries: &mut Vec<UsageTrendSummary>,
    entries: &[UsageLogSample],
    label: &'static str,
    value: fn(&UsageLogSample) -> Option<f64>,
) {
    let Some(latest) = entries.last() else {
        return;
    };
    let Some(latest_value) = value(latest) else {
        return;
    };
    let Some(previous) = entries.iter().rev().skip(1).find(|entry| {
        value(entry).is_some()
            && latest
                .timestamp
                .signed_duration_since(entry.timestamp)
                .num_seconds()
                >= 30
    }) else {
        return;
    };
    let Some(previous_value) = value(previous) else {
        return;
    };

    let elapsed_hours = latest
        .timestamp
        .signed_duration_since(previous.timestamp)
        .num_seconds()
        .max(1) as f64
        / 3600.0;
    let delta_percent = latest_value - previous_value;
    let per_hour = delta_percent / elapsed_hours;
    let average_per_day_24h = average_positive_delta_per_day(entries, latest.timestamp, 1, value);
    let average_per_day_7d = average_positive_delta_per_day(entries, latest.timestamp, 7, value);
    let comparison = compare_current_to_average(delta_percent, per_hour, average_per_day_7d);
    let sample_count = entries.iter().filter(|entry| value(entry).is_some()).count();

    summaries.push(UsageTrendSummary {
        label,
        delta_percent,
        per_hour,
        average_per_day_24h,
        average_per_day_7d,
        comparison,
        sample_count,
    });
}

fn average_positive_delta_per_day(
    entries: &[UsageLogSample],
    now: DateTime<Utc>,
    days: i64,
    value: fn(&UsageLogSample) -> Option<f64>,
) -> Option<f64> {
    let cutoff = now - Duration::days(days);
    let window: Vec<&UsageLogSample> = entries
        .iter()
        .filter(|entry| entry.timestamp >= cutoff && value(entry).is_some())
        .collect();
    if window.len() < 2 {
        return None;
    }

    let mut total_delta = 0.0;
    for pair in window.windows(2) {
        let Some(previous) = value(pair[0]) else {
            continue;
        };
        let Some(current) = value(pair[1]) else {
            continue;
        };
        let delta = current - previous;
        if delta > 0.0 {
            total_delta += delta;
        }
    }

    let elapsed_days = window
        .last()?
        .timestamp
        .signed_duration_since(window.first()?.timestamp)
        .num_seconds()
        .max(1) as f64
        / 86_400.0;
    Some(total_delta / elapsed_days)
}

fn compare_current_to_average(
    delta_percent: f64,
    per_hour: f64,
    average_per_day_7d: Option<f64>,
) -> UsageTrendComparison {
    if delta_percent < -0.1 {
        return UsageTrendComparison::ResetOrDrop;
    }

    let Some(average_per_day_7d) = average_per_day_7d else {
        return UsageTrendComparison::Insufficient;
    };
    let current_per_day = per_hour.max(0.0) * 24.0;
    let diff = current_per_day - average_per_day_7d;
    let relative_threshold = (average_per_day_7d.abs() * 0.25).max(1.0);

    if diff > relative_threshold {
        UsageTrendComparison::Faster
    } else if diff < -relative_threshold {
        UsageTrendComparison::Slower
    } else {
        UsageTrendComparison::Steady
    }
}
