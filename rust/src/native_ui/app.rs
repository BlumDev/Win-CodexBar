//! Main egui application - Modern refined menubar popup
//! Clean, spacious design with rich visual hierarchy

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, Rect, RichText, Rounding, Stroke, Vec2,
};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::charts::{
    ChartPoint, CostHistoryChart, CreditsHistoryChart, ServiceUsage, UsageBreakdownChart,
    UsageBreakdownPoint,
};
use super::preferences::PreferencesWindow;
use super::provider_icons::ProviderIconCache;
use super::theme::{FontSize, Radius, Spacing, Theme, provider_color, provider_icon, status_color};
use crate::browser::cookies::get_cookie_header;
use crate::core::{
    CodexTokenSnapshot, FetchContext, JsonlScanner, OpenAIDashboardCacheStore,
    PersonalInfoRedactor, Provider, ProviderFetchResult, ProviderId, RateWindow,
};
use crate::core::{TokenAccountStore, TokenAccountSupport};
use crate::cost_scanner::get_daily_cost_history;
use crate::login::LoginPhase;
use crate::providers::*;
use crate::settings::{ApiKeys, ManualCookies, Settings};
use crate::shortcuts::{ShortcutManager, parse_shortcut};
use crate::status::{StatusLevel, fetch_provider_status, get_status_page_url};
use crate::tray::{
    LoadingPattern, ProviderUsage, SurpriseAnimation, TrayMenuAction, UnifiedTrayManager,
};
use crate::updater::{self, UpdateInfo, UpdateState};
use crate::usage_log::{
    CodexTokenLogSample, CodexTokenModelLogTotals, UsageLogSample, UsageTrendComparison,
    UsageTrendSummary, append_and_summarize_usage,
};

#[cfg(windows)]
fn restore_main_window() {
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, IsIconic, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
    };
    use windows::core::w;

    unsafe {
        if let Ok(hwnd) = FindWindowW(None, w!("CodexBar")) {
            if !hwnd.is_invalid() {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                } else {
                    let _ = ShowWindow(hwnd, SW_SHOW);
                }
                let _ = SetForegroundWindow(hwnd);
            }
        }
    }
}

#[cfg(windows)]
fn show_main_window_no_focus() {
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SW_SHOWNOACTIVATE, ShowWindow};
    use windows::core::w;

    unsafe {
        if let Ok(hwnd) = FindWindowW(None, w!("CodexBar")) {
            if !hwnd.is_invalid() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
}

#[cfg(not(windows))]
fn show_main_window_no_focus() {}

#[cfg(not(windows))]
fn restore_main_window() {}

#[derive(Clone, Debug)]
pub struct ProviderData {
    pub name: String,
    pub display_name: String,
    pub account: Option<String>, // Account email for display
    pub source_label: Option<String>,
    pub is_loading: bool,
    pub session_percent: Option<f64>,
    pub session_reset: Option<String>,
    pub weekly_percent: Option<f64>,
    pub weekly_reset: Option<String>,
    pub model_percent: Option<f64>,
    pub model_reset: Option<String>,
    pub model_name: Option<String>,
    pub plan: Option<String>,
    pub error: Option<String>,
    pub dashboard_url: Option<String>,
    pub pace_percent: Option<f64>,
    pub pace_lasts_to_reset: bool,
    pub session_pace_percent: Option<f64>,
    pub session_pace_lasts_to_reset: bool,
    pub weekly_pace_percent: Option<f64>,
    pub weekly_pace_lasts_to_reset: bool,
    pub model_pace_percent: Option<f64>,
    pub model_pace_lasts_to_reset: bool,
    pub cost_used: Option<String>,
    pub credits_remaining: Option<f64>,
    pub credits_percent: Option<f64>,
    pub status_level: StatusLevel,
    pub status_description: Option<String>,
    pub cost_history: Vec<(String, f64)>,
    pub credits_history: Vec<(String, f64)>,
    pub usage_breakdown: Vec<UsageBreakdownPoint>,
    pub usage_trends: Vec<UsageTrendSummary>,
    pub codex_tokens: Option<CodexTokenLogSample>,
    codex_window_ratio: Option<CodexWindowRatio>,
}

#[derive(Debug, Clone, Copy)]
struct CodexUsageSample {
    session_used: f64,
    weekly_used: f64,
}

#[derive(Debug, Clone, Copy)]
struct CodexWindowRatio {
    weekly_per_full_session: f64,
    estimated_windows: f64,
}

impl ProviderData {
    fn placeholder(id: ProviderId) -> Self {
        Self {
            name: id.cli_name().to_string(),
            display_name: id.display_name().to_string(),
            account: None,
            source_label: None,
            is_loading: true,
            session_percent: None,
            session_reset: None,
            weekly_percent: None,
            weekly_reset: None,
            model_percent: None,
            model_reset: None,
            model_name: None,
            plan: None,
            error: None,
            dashboard_url: None,
            pace_percent: None,
            pace_lasts_to_reset: false,
            session_pace_percent: None,
            session_pace_lasts_to_reset: false,
            weekly_pace_percent: None,
            weekly_pace_lasts_to_reset: false,
            model_pace_percent: None,
            model_pace_lasts_to_reset: false,
            cost_used: None,
            credits_remaining: None,
            credits_percent: None,
            status_level: StatusLevel::Unknown,
            status_description: None,
            cost_history: Vec::new(),
            credits_history: Vec::new(),
            usage_breakdown: Vec::new(),
            usage_trends: Vec::new(),
            codex_tokens: None,
            codex_window_ratio: None,
        }
    }

    fn from_result(
        id: ProviderId,
        result: &ProviderFetchResult,
        metadata: &crate::core::ProviderMetadata,
        reset_time_relative: bool,
    ) -> Self {
        let snapshot = &result.usage;
        let (session_pace_percent, session_pace_lasts) = calculate_pace(&snapshot.primary);
        let (weekly_pace_percent, weekly_pace_lasts) = snapshot
            .secondary
            .as_ref()
            .map(calculate_pace)
            .unwrap_or((None, false));
        let (model_pace_percent, model_pace_lasts) = snapshot
            .model_specific
            .as_ref()
            .map(calculate_pace)
            .unwrap_or((None, false));

        let (cost_used, credits_remaining, credits_percent) = if let Some(ref cost) = result.cost {
            if cost.period == "Credits" {
                // Use the limit from the cost snapshot if available, otherwise default to 1000
                let scale = cost.limit.unwrap_or(1000.0);
                let remaining = cost.used;
                let percent = if scale > 0.0 {
                    (remaining / scale * 100.0).clamp(0.0, 100.0)
                } else {
                    0.0
                };
                (None, Some(remaining), Some(percent))
            } else {
                (Some(cost.format_used()), None, None)
            }
        } else {
            (None, None, None)
        };

        Self {
            name: id.cli_name().to_string(),
            display_name: id.display_name().to_string(),
            account: snapshot.account_email.clone(), // Account email if available
            source_label: Some(result.source_label.clone()),
            is_loading: false,
            session_percent: Some(snapshot.primary.used_percent),
            session_reset: snapshot
                .primary
                .resets_at
                .map(|t| format_reset_time(t, reset_time_relative)),
            weekly_percent: snapshot.secondary.as_ref().map(|s| s.used_percent),
            weekly_reset: snapshot.secondary.as_ref().and_then(|s| {
                s.resets_at
                    .map(|t| format_reset_time(t, reset_time_relative))
            }),
            model_percent: snapshot.model_specific.as_ref().map(|m| m.used_percent),
            model_reset: snapshot.model_specific.as_ref().and_then(|m| {
                m.resets_at
                    .map(|t| format_reset_time(t, reset_time_relative))
            }),
            model_name: if id == ProviderId::Gemini && snapshot.model_specific.is_some() {
                Some("Other Models".to_string())
            } else {
                snapshot
                    .model_specific
                    .as_ref()
                    .and_then(|m| m.reset_description.clone())
            },
            plan: snapshot.login_method.clone(),
            error: None,
            dashboard_url: metadata.dashboard_url.map(|s| s.to_string()),
            pace_percent: weekly_pace_percent,
            pace_lasts_to_reset: weekly_pace_lasts,
            session_pace_percent,
            session_pace_lasts_to_reset: session_pace_lasts,
            weekly_pace_percent,
            weekly_pace_lasts_to_reset: weekly_pace_lasts,
            model_pace_percent,
            model_pace_lasts_to_reset: model_pace_lasts,
            cost_used,
            credits_remaining,
            credits_percent,
            status_level: StatusLevel::Unknown,
            status_description: None,
            cost_history: Vec::new(),
            credits_history: Vec::new(),
            usage_breakdown: Vec::new(),
            usage_trends: Vec::new(),
            codex_tokens: None,
            codex_window_ratio: None,
        }
    }

    fn from_error(id: ProviderId, error: String) -> Self {
        Self {
            name: id.cli_name().to_string(),
            display_name: id.display_name().to_string(),
            account: None,
            source_label: None,
            is_loading: false,
            session_percent: None,
            session_reset: None,
            weekly_percent: None,
            weekly_reset: None,
            model_percent: None,
            model_reset: None,
            model_name: None,
            plan: None,
            error: Some(error),
            dashboard_url: None,
            pace_percent: None,
            pace_lasts_to_reset: false,
            session_pace_percent: None,
            session_pace_lasts_to_reset: false,
            weekly_pace_percent: None,
            weekly_pace_lasts_to_reset: false,
            model_pace_percent: None,
            model_pace_lasts_to_reset: false,
            cost_used: None,
            credits_remaining: None,
            credits_percent: None,
            status_level: StatusLevel::Unknown,
            status_description: None,
            cost_history: Vec::new(),
            credits_history: Vec::new(),
            usage_breakdown: Vec::new(),
            usage_trends: Vec::new(),
            codex_tokens: None,
            codex_window_ratio: None,
        }
    }

    /// Get the preferred metric percent based on the MetricPreference setting
    pub fn get_preferred_metric(&self, pref: crate::settings::MetricPreference) -> f64 {
        match pref {
            crate::settings::MetricPreference::Session => self.session_percent.unwrap_or(0.0),
            crate::settings::MetricPreference::Weekly => self
                .weekly_percent
                .unwrap_or_else(|| self.session_percent.unwrap_or(0.0)),
            crate::settings::MetricPreference::Model => self
                .model_percent
                .unwrap_or_else(|| self.session_percent.unwrap_or(0.0)),
            crate::settings::MetricPreference::Credits => {
                // For credits, we show the credits_percent (remaining as percentage of full scale)
                self.credits_percent
                    .unwrap_or_else(|| self.session_percent.unwrap_or(0.0))
            }
            crate::settings::MetricPreference::Average => {
                // Average of all available metrics
                let mut sum = 0.0;
                let mut count = 0;
                if let Some(v) = self.session_percent {
                    sum += v;
                    count += 1;
                }
                if let Some(v) = self.weekly_percent {
                    sum += v;
                    count += 1;
                }
                if let Some(v) = self.model_percent {
                    sum += v;
                    count += 1;
                }
                if count > 0 { sum / count as f64 } else { 0.0 }
            }
            crate::settings::MetricPreference::Automatic => {
                // Automatic: prefer the highest available metric (most concerning)
                let session = self.session_percent.unwrap_or(0.0);
                let weekly = self.weekly_percent.unwrap_or(0.0);
                let model = self.model_percent.unwrap_or(0.0);
                session.max(weekly).max(model)
            }
        }
    }
}

fn format_reset_time(reset: chrono::DateTime<chrono::Utc>, relative: bool) -> String {
    if relative {
        let now = chrono::Utc::now();
        let diff = reset - now;

        if diff.num_seconds() <= 0 {
            return "Resetting...".to_string();
        }

        let hours = diff.num_hours();
        let minutes = (diff.num_minutes() % 60).abs();

        if hours >= 24 {
            let days = hours / 24;
            let remaining_hours = hours % 24;
            format!("{}d {}h", days, remaining_hours)
        } else {
            format!("{}h {}m", hours, minutes)
        }
    } else {
        // Absolute time format using local timezone
        // Include date if not today
        use chrono::Local;
        let local_time = reset.with_timezone(&Local);
        let today = Local::now().date_naive();
        let reset_date = local_time.date_naive();

        if reset_date == today {
            local_time.format("%I:%M %p").to_string()
        } else if reset_date == today + chrono::Days::new(1) {
            format!("Tomorrow {}", local_time.format("%I:%M %p"))
        } else {
            local_time.format("%b %d, %I:%M %p").to_string()
        }
    }
}

fn calculate_pace(rate_window: &RateWindow) -> (Option<f64>, bool) {
    let Some(window_minutes) = rate_window.window_minutes else {
        return (None, false);
    };
    let Some(resets_at) = rate_window.resets_at else {
        return (None, false);
    };

    let now = chrono::Utc::now();
    let time_remaining = resets_at - now;
    let remaining_minutes = time_remaining.num_minutes() as f64;

    if remaining_minutes <= 0.0 {
        return (None, false);
    }

    let total_minutes = window_minutes as f64;
    let elapsed_minutes = total_minutes - remaining_minutes;

    if elapsed_minutes <= 0.0 {
        return (None, false);
    }

    let expected_percent = (elapsed_minutes / total_minutes) * 100.0;
    let actual_percent = rate_window.used_percent;
    let pace = actual_percent - expected_percent;
    let lasts_to_reset = actual_percent <= expected_percent;

    (Some(pace), lasts_to_reset)
}

fn usage_display_percent(used_percent: f64, show_as_used: bool) -> f64 {
    let used_percent = used_percent.clamp(0.0, 100.0);
    if show_as_used {
        used_percent
    } else {
        100.0 - used_percent
    }
}

fn usage_display_label(display_percent: f64, show_as_used: bool) -> String {
    if show_as_used {
        format!("{:.0}% used", display_percent)
    } else {
        format!("{:.0}% remaining", display_percent)
    }
}

fn update_codex_window_ratio(
    previous: Option<CodexUsageSample>,
    current: CodexUsageSample,
    existing: Option<CodexWindowRatio>,
) -> Option<CodexWindowRatio> {
    let previous = previous?;
    let session_delta = current.session_used - previous.session_used;
    let weekly_delta = current.weekly_used - previous.weekly_used;

    if session_delta <= 0.5 || weekly_delta <= 0.05 {
        return existing;
    }

    let observed_windows = (session_delta / weekly_delta).clamp(1.0, 50.0);
    let estimated_windows = existing
        .map(|ratio| (ratio.estimated_windows * 0.7) + (observed_windows * 0.3))
        .unwrap_or(observed_windows);
    let weekly_per_full_session = (100.0 / estimated_windows).clamp(0.1, 100.0);

    Some(CodexWindowRatio {
        weekly_per_full_session,
        estimated_windows,
    })
}

fn codex_window_ratio_label(ratio: Option<CodexWindowRatio>) -> (String, Option<String>, Color32) {
    if let Some(ratio) = ratio {
        (
            format!("~{:.0}% weekly", ratio.weekly_per_full_session),
            Some(format!("~{:.1}x/week", ratio.estimated_windows)),
            metric_severity_color(ratio.weekly_per_full_session),
        )
    } else {
        (
            "No ratio yet".to_string(),
            Some("after usage".to_string()),
            Theme::TEXT_SECONDARY,
        )
    }
}

fn pace_status(
    display_percent: f64,
    pace_percent: Option<f64>,
    pace_lasts_to_reset: bool,
) -> Option<(&'static str, Color32)> {
    pace_status_with_mode(display_percent, pace_percent, pace_lasts_to_reset)
}

fn cursor_metric_color(
    display_percent: f64,
    _pace_percent: Option<f64>,
    _pace_lasts_to_reset: bool,
) -> Color32 {
    metric_severity_color(display_percent)
}

fn cursor_reset_time_color(used_percent: f64, pace_percent: Option<f64>) -> Option<Color32> {
    let pace = pace_percent?;
    let elapsed_percent = (used_percent - pace).clamp(0.0, 100.0);
    let remaining_percent = 100.0 - elapsed_percent;
    Some(if remaining_percent <= 5.0 {
        Theme::RED
    } else if remaining_percent <= 12.0 {
        Theme::ORANGE
    } else if remaining_percent <= 25.0 {
        Theme::YELLOW
    } else {
        Theme::GREEN
    })
}

fn pace_status_with_mode(
    display_percent: f64,
    pace_percent: Option<f64>,
    pace_lasts_to_reset: bool,
) -> Option<(&'static str, Color32)> {
    let delta = pace_percent?;

    if pace_lasts_to_reset {
        return Some(("OK", Theme::GREEN));
    }

    let color = pace_severity_color(display_percent, Some(delta));
    let label = if delta > 25.0 {
        "Critical"
    } else if delta > 12.0 {
        "Fast"
    } else {
        "Ahead"
    };
    Some((label, color))
}

fn metric_severity_color(display_percent: f64) -> Color32 {
    Theme::usage_color(display_percent.clamp(0.0, 100.0))
}

fn severity_rank(color: Color32) -> u8 {
    if color == Theme::GREEN {
        0
    } else if color == Theme::YELLOW {
        1
    } else if color == Theme::ORANGE {
        2
    } else {
        3
    }
}

fn pace_severity_color(display_percent: f64, pace_percent: Option<f64>) -> Color32 {
    let usage_color = metric_severity_color(display_percent);
    let pace_color = if let Some(delta) = pace_percent {
        let ahead = delta.max(0.0);
        if ahead <= 2.0 {
            Theme::GREEN
        } else if ahead <= 8.0 {
            Theme::YELLOW
        } else if ahead <= 15.0 {
            Theme::ORANGE
        } else {
            Theme::RED
        }
    } else {
        usage_color
    };

    if severity_rank(pace_color) >= severity_rank(usage_color) {
        pace_color
    } else {
        usage_color
    }
}

fn tinted(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn load_usage_breakdown_points(
    provider_id: ProviderId,
    account_email: Option<&str>,
) -> Vec<UsageBreakdownPoint> {
    if provider_id != ProviderId::Codex {
        return Vec::new();
    }

    // Require account_email to validate cache belongs to current account
    // Without it, we risk showing stale data from a different account
    let Some(account_email) = account_email else {
        return Vec::new();
    };

    let Some(cache) = OpenAIDashboardCacheStore::load() else {
        return Vec::new();
    };

    // Verify cache belongs to current account
    if !cache.account_email.eq_ignore_ascii_case(account_email) {
        return Vec::new();
    }

    cache
        .snapshot
        .usage_breakdown
        .iter()
        .map(|point| {
            let services = point
                .services
                .iter()
                .map(|service| ServiceUsage {
                    service: service.service.clone(),
                    credits_used: service.credits_used,
                })
                .collect();
            UsageBreakdownPoint::new(point.day.clone(), services)
        })
        .collect()
}

fn load_credits_history_points(
    provider_id: ProviderId,
    account_email: Option<&str>,
) -> Vec<(String, f64)> {
    if provider_id != ProviderId::Codex {
        return Vec::new();
    }

    let Some(account_email) = account_email else {
        return Vec::new();
    };

    let Some(cache) = OpenAIDashboardCacheStore::load() else {
        return Vec::new();
    };

    if !cache.account_email.eq_ignore_ascii_case(account_email) {
        return Vec::new();
    }

    let breakdown = if !cache.snapshot.daily_breakdown.is_empty() {
        &cache.snapshot.daily_breakdown
    } else if !cache.snapshot.usage_breakdown.is_empty() {
        &cache.snapshot.usage_breakdown
    } else {
        return Vec::new();
    };

    let mut points: Vec<(String, f64)> = breakdown
        .iter()
        .map(|day| (day.day.clone(), day.total_credits_used))
        .collect();

    points.sort_by(|a, b| a.0.cmp(&b.0));
    points
}

fn codex_token_log_sample(snapshot: CodexTokenSnapshot) -> CodexTokenLogSample {
    let by_model_total = snapshot
        .by_model
        .into_iter()
        .map(|(model, totals)| {
            (
                model,
                CodexTokenModelLogTotals {
                    input_tokens: totals.input_tokens,
                    cached_tokens: totals.cached_tokens,
                    output_tokens: totals.output_tokens,
                    total_tokens: totals.total_tokens,
                },
            )
        })
        .collect();

    CodexTokenLogSample {
        last_model: snapshot.last_model,
        input_tokens_total: snapshot.input_tokens,
        cached_tokens_total: snapshot.cached_tokens,
        output_tokens_total: snapshot.output_tokens,
        total_tokens_total: snapshot.total_tokens,
        input_tokens_delta: 0,
        cached_tokens_delta: 0,
        output_tokens_delta: 0,
        total_tokens_delta: 0,
        by_model_total,
    }
}

fn random_surprise_delay() -> Duration {
    use rand::Rng;
    let mut rng = rand::rng();
    let secs = rng.random_range(30..300);
    Duration::from_secs(secs)
}

struct SharedState {
    providers: Vec<ProviderData>,
    selected_provider_idx: usize, // Index of selected provider in grid
    last_codex_sample: Option<CodexUsageSample>,
    codex_window_ratio: Option<CodexWindowRatio>,
    last_refresh: Instant,
    is_refreshing: bool,
    loading_pattern: LoadingPattern,
    loading_phase: f64,
    surprise_animation: Option<SurpriseAnimation>,
    surprise_frame: u32,
    next_surprise_time: Instant,
    update_available: Option<UpdateInfo>,
    update_checked: bool,
    update_dismissed: bool,
    update_state: UpdateState,
    login_provider: Option<String>,
    login_phase: LoginPhase,
    login_message: Option<String>,
}

pub struct CodexBarApp {
    state: Arc<Mutex<SharedState>>,
    settings: Settings,
    tray_manager: Option<UnifiedTrayManager>,
    tray_action_rx: Option<Receiver<TrayMenuAction>>,
    preferences_window: PreferencesWindow,
    shortcut_manager: Option<ShortcutManager>,
    icon_cache: ProviderIconCache,
    was_refreshing: bool, // Track previous frame's refresh state
    pending_main_window_layout: bool,
    anchor_main_window_to_pointer: bool,
    #[cfg(debug_assertions)]
    test_input_queue: super::test_server::TestInputQueue,
}

impl CodexBarApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Load Windows symbol font
        let mut fonts = FontDefinitions::default();
        if let Ok(font_data) = std::fs::read("C:\\Windows\\Fonts\\seguisym.ttf") {
            fonts.font_data.insert(
                "segoe_symbols".to_owned(),
                FontData::from_owned(font_data).into(),
            );
            fonts
                .families
                .entry(FontFamily::Proportional)
                .or_default()
                .push("segoe_symbols".to_owned());
        }
        cc.egui_ctx.set_fonts(fonts);

        let settings = Settings::load();
        let enabled_ids = settings.get_enabled_provider_ids();

        let placeholders: Vec<ProviderData> = enabled_ids
            .iter()
            .map(|&id| ProviderData::placeholder(id))
            .collect();

        let state = Arc::new(Mutex::new(SharedState {
            providers: placeholders,
            selected_provider_idx: 0, // Select first provider by default
            last_codex_sample: None,
            codex_window_ratio: None,
            last_refresh: Instant::now()
                .checked_sub(Duration::from_secs(999))
                .unwrap_or_else(Instant::now),
            is_refreshing: false,
            loading_pattern: LoadingPattern::random(),
            loading_phase: 0.0,
            surprise_animation: None,
            surprise_frame: 0,
            next_surprise_time: Instant::now() + random_surprise_delay(),
            update_available: None,
            update_checked: false,
            update_dismissed: false,
            update_state: UpdateState::Idle,
            login_provider: None,
            login_phase: LoginPhase::Idle,
            login_message: None,
        }));

        // Initialize system tray based on settings
        let tray_manager = match UnifiedTrayManager::new(&settings) {
            Ok(tm) => Some(tm),
            Err(e) => {
                tracing::warn!("Failed to create tray manager: {}", e);
                None
            }
        };
        let tray_action_rx = if tray_manager.is_some() {
            let (tx, rx) = mpsc::channel::<TrayMenuAction>();
            let repaint_ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                loop {
                    if let Some(action) = UnifiedTrayManager::check_events() {
                        if matches!(action, TrayMenuAction::Open | TrayMenuAction::Refresh) {
                            // Egui viewport commands alone can be ignored while minimized.
                            // Force a native restore first so the update loop wakes up.
                            restore_main_window();
                            repaint_ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                            repaint_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        } else if matches!(action, TrayMenuAction::Settings) {
                            // Show main window so update() runs (needed to spawn the
                            // settings child viewport), but don't steal focus.
                            show_main_window_no_focus();
                            repaint_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        }
                        if tx.send(action).is_err() {
                            break;
                        }
                        repaint_ctx.request_repaint();
                    } else {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            });
            Some(rx)
        } else {
            None
        };

        // Check for updates in background (using configured update channel)
        {
            let state = Arc::clone(&state);
            let update_channel = settings.update_channel;
            let auto_download = settings.auto_download_updates;
            let ignored_update_version = settings.ignored_update_version.clone();
            std::thread::spawn(move || {
                let rt = match tokio::runtime::Runtime::new() {
                    Ok(rt) => rt,
                    Err(e) => {
                        tracing::error!("Failed to create tokio runtime for update check: {}", e);
                        return;
                    }
                };
                rt.block_on(async {
                    if let Some(update) =
                        updater::check_for_updates_with_channel(update_channel).await
                    {
                        if ignored_update_version.as_deref() == Some(update.version.as_str()) {
                            if let Ok(mut s) = state.lock() {
                                s.update_available = None;
                                s.update_checked = true;
                                s.update_state = UpdateState::Idle;
                            }
                            return;
                        }

                        let should_download = {
                            if let Ok(mut s) = state.lock() {
                                s.update_available = Some(update.clone());
                                s.update_checked = true;
                                s.update_state = UpdateState::Available;
                                auto_download
                            } else {
                                false
                            }
                        };

                        // Start background download if auto-download is enabled
                        if should_download {
                            let (progress_tx, mut progress_rx) =
                                tokio::sync::watch::channel(UpdateState::Available);
                            let state_clone = Arc::clone(&state);

                            // Update state to downloading
                            if let Ok(mut s) = state_clone.lock() {
                                s.update_state = UpdateState::Downloading(0.0);
                            }

                            // Spawn a task to monitor progress updates
                            let progress_state = Arc::clone(&state_clone);
                            tokio::spawn(async move {
                                while progress_rx.changed().await.is_ok() {
                                    let new_state = progress_rx.borrow().clone();
                                    if let Ok(mut s) = progress_state.lock() {
                                        s.update_state = new_state;
                                    }
                                }
                            });

                            match updater::download_update(&update, progress_tx).await {
                                Ok(path) => {
                                    if let Ok(mut s) = state_clone.lock() {
                                        s.update_state = UpdateState::Ready(path);
                                    }
                                }
                                Err(e) => {
                                    tracing::error!("Failed to download update: {}", e);
                                    if let Ok(mut s) = state_clone.lock() {
                                        s.update_state = UpdateState::Failed(e);
                                    }
                                }
                            }
                        }
                    } else if let Ok(mut s) = state.lock() {
                        s.update_checked = true;
                    }
                });
            });
        }

        // Initialize keyboard shortcuts with custom shortcut from settings
        let shortcut_manager = match ShortcutManager::new() {
            Ok(mut sm) => {
                // Apply custom shortcut from settings if configured
                if let Some((modifiers, key)) = parse_shortcut(&settings.global_shortcut) {
                    if let Err(e) = sm.set_open_menu_shortcut(modifiers, key) {
                        tracing::warn!(
                            "Failed to set custom shortcut '{}': {}",
                            settings.global_shortcut,
                            e
                        );
                    } else {
                        tracing::info!(
                            "Keyboard shortcut registered: {}",
                            settings.global_shortcut
                        );
                    }
                } else {
                    tracing::info!("Keyboard shortcut registered: Ctrl+Shift+U (default)");
                }
                Some(sm)
            }
            Err(e) => {
                tracing::warn!("Failed to register keyboard shortcuts: {}", e);
                None
            }
        };

        // Initialize test input queue and start server (debug builds only)
        #[cfg(debug_assertions)]
        let test_input_queue = {
            let q = super::test_server::create_queue();
            super::test_server::start_server(q.clone());
            q
        };

        Self {
            state,
            settings,
            tray_manager,
            tray_action_rx,
            preferences_window: PreferencesWindow::new(),
            shortcut_manager,
            icon_cache: ProviderIconCache::new(),
            was_refreshing: false,
            pending_main_window_layout: false,
            anchor_main_window_to_pointer: false,
            #[cfg(debug_assertions)]
            test_input_queue,
        }
    }

    fn layout_main_window(&mut self, ctx: &egui::Context, anchor_to_pointer: bool) {
        let Some(outer_rect) = ctx.input(|i| i.viewport().outer_rect) else {
            return;
        };
        let Some(work_area) = work_area_rect(ctx) else {
            return;
        };

        let margin = 12.0;
        let gap = 10.0;
        let min_size = egui::vec2(460.0, 540.0);
        let max_w = (work_area.width() - margin * 2.0).max(min_size.x);
        let max_h = (work_area.height() - margin * 2.0).max(min_size.y);
        let target_size = egui::vec2(560.0_f32.min(max_w), 720.0_f32.min(max_h));
        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(target_size));

        let anchor = if anchor_to_pointer {
            ctx.input(|i| i.pointer.latest_pos())
        } else {
            None
        }
        .unwrap_or_else(|| outer_rect.center());

        // For tray/shortcut opens, keep the popup on the left side and vertically centered
        // so it doesn't appear pinned to the taskbar area.
        let (target_x, target_y) = if anchor_to_pointer {
            let center_x = work_area.min.x + work_area.width() * 0.22;
            (
                center_x - target_size.x * 0.5,
                work_area.min.y + (work_area.height() - target_size.y) * 0.5,
            )
        } else {
            let space_above = anchor.y - work_area.min.y - margin;
            let space_below = work_area.max.y - anchor.y - margin;
            let x = anchor.x - target_size.x * 0.5;
            let y = if space_below >= target_size.y + gap || space_below >= space_above {
                anchor.y + gap
            } else {
                anchor.y - target_size.y - gap
            };
            (x, y)
        };

        let min_x = work_area.min.x + margin;
        let min_y = work_area.min.y + margin;
        let max_x = (work_area.max.x - target_size.x - margin).max(min_x);
        let max_y = (work_area.max.y - target_size.y - margin).max(min_y);
        let x = if max_x <= min_x {
            min_x
        } else {
            target_x.clamp(min_x, max_x)
        };
        let y = if max_y <= min_y {
            min_y
        } else {
            target_y.clamp(min_y, max_y)
        };

        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(x, y)));
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);

        self.pending_main_window_layout = false;
        self.anchor_main_window_to_pointer = false;
    }

    fn refresh_providers(&self) {
        let state = Arc::clone(&self.state);
        let enabled_ids = self.settings.get_enabled_provider_ids();
        let manual_cookies = ManualCookies::load();
        let api_keys = ApiKeys::load();
        let reset_time_relative = self.settings.reset_time_relative;
        // Load token accounts for account switching support
        let token_accounts = TokenAccountStore::new().load().unwrap_or_default();

        std::thread::spawn(move || {
            if let Ok(mut s) = state.lock() {
                s.is_refreshing = true;
                s.loading_pattern = LoadingPattern::random();
                s.loading_phase = 0.0;
                // Only rebuild the provider list when the enabled set actually
                // changed (first load, or a provider toggled on/off). Otherwise
                // keep the existing entries so the dashboard updates in place
                // instead of flashing placeholders (0% / blank) on every refresh.
                let same_set = s.providers.len() == enabled_ids.len()
                    && s.providers
                        .iter()
                        .zip(enabled_ids.iter())
                        .all(|(existing, &id)| existing.name == id.cli_name());
                if !same_set {
                    s.providers = enabled_ids
                        .iter()
                        .map(|&id| ProviderData::placeholder(id))
                        .collect();
                } else {
                    for provider in &mut s.providers {
                        provider.is_loading = true;
                    }
                }
                if s.selected_provider_idx >= s.providers.len() {
                    s.selected_provider_idx = 0;
                }
            }

            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("Failed to create tokio runtime: {}", e);
                    if let Ok(mut s) = state.lock() {
                        s.is_refreshing = false;
                        for provider in &mut s.providers {
                            provider.is_loading = false;
                        }
                    }
                    return;
                }
            };
            rt.block_on(async {
                // Clear any stale OAuth env vars at the start of refresh
                // This ensures account switches take effect immediately
                const OAUTH_ENV_KEYS: &[&str] = &[
                    "CODEXBAR_CLAUDE_OAUTH_TOKEN",
                    "ZED_API_TOKEN",
                    "ZAI_API_TOKEN",
                ];
                for key in OAUTH_ENV_KEYS {
                    // SAFETY: no concurrent threads are reading these env vars at this point
                    unsafe { std::env::remove_var(key) };
                }

                let handles: Vec<_> = enabled_ids
                    .iter()
                    .enumerate()
                    .map(|(idx, &id)| {
                        // Check for active token account first
                        let active_token = token_accounts
                            .get(&id)
                            .and_then(|data| data.active_account())
                            .map(|account| account.token.clone());

                        // Check for environment override from token account (e.g., for Zai/Claude OAuth)
                        let env_override = active_token
                            .as_ref()
                            .and_then(|token| TokenAccountSupport::env_override(id, token));

                        // Set env override if present - providers will read from env vars
                        // Note: We don't clear env vars here to avoid race conditions with
                        // concurrent provider fetches. Instead, we pass api_key directly
                        // through FetchContext for providers that support it.
                        if let Some(ref env_vars) = env_override {
                            for (key, value) in env_vars {
                                // SAFETY: called sequentially before spawning provider fetch tasks
                                unsafe { std::env::set_var(key, value) };
                            }
                        }

                        // Determine cookie header: active token account > manual cookie > browser extraction
                        let cookie_header = if env_override.is_some() {
                            None
                        } else if let Some(ref token) = active_token {
                            // Use active account's token, normalized for this provider
                            Some(TokenAccountSupport::normalized_cookie_header(id, token))
                        } else {
                            // Fallback to manual cookie or browser extraction
                            let manual_cookie =
                                manual_cookies.get(id.cli_name()).map(|s| s.to_string());
                            manual_cookie.or_else(|| {
                                // Try browser cookie extraction if no manual cookie
                                id.cookie_domain().and_then(|domain| {
                                    get_cookie_header(domain).ok().filter(|h| !h.is_empty())
                                })
                            })
                        };

                        let api_key = if env_override.is_some() {
                            // If we have env override, extract API key from it
                            env_override
                                .as_ref()
                                .and_then(|env| env.values().next().cloned())
                        } else {
                            api_keys.get(id.cli_name()).map(|s| s.to_string())
                        };

                        let ctx = FetchContext {
                            manual_cookie_header: cookie_header,
                            api_key,
                            ..FetchContext::default()
                        };
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            let provider = create_provider(id);
                            let metadata = provider.metadata().clone();
                            let provider_name = id.cli_name().to_string();

                            let (usage_result, status_result) = tokio::join!(
                                async {
                                    tokio::time::timeout(
                                        std::time::Duration::from_secs(5),
                                        provider.fetch_usage(&ctx),
                                    )
                                    .await
                                },
                                async {
                                    tokio::time::timeout(
                                        std::time::Duration::from_secs(5),
                                        fetch_provider_status(&provider_name),
                                    )
                                    .await
                                }
                            );

                            let mut result = match usage_result {
                                Ok(Ok(result)) => ProviderData::from_result(
                                    id,
                                    &result,
                                    &metadata,
                                    reset_time_relative,
                                ),
                                Ok(Err(e)) => ProviderData::from_error(id, e.to_string()),
                                Err(_) => ProviderData::from_error(id, "Timeout".to_string()),
                            };

                            result.is_loading = false;

                            if let Ok(Some(status)) = status_result {
                                result.status_level = status.level;
                                result.status_description = Some(status.description);
                            }

                            if result.error.is_none() {
                                result.usage_breakdown =
                                    load_usage_breakdown_points(id, result.account.as_deref());
                            }

                            let provider_name_lower = provider_name.to_lowercase();
                            if provider_name_lower == "codex" || provider_name_lower == "claude" {
                                result.cost_history =
                                    get_daily_cost_history(&provider_name_lower, 30);
                            }

                            if result.error.is_none() {
                                result.credits_history =
                                    load_credits_history_points(id, result.account.as_deref());
                            }

                            if let Ok(mut s) = state.lock() {
                                if idx < s.providers.len() {
                                    if result.error.is_none() {
                                        let codex_tokens = if id == ProviderId::Codex {
                                            JsonlScanner::codex_token_snapshot(30)
                                                .map(codex_token_log_sample)
                                        } else {
                                            None
                                        };
                                        result.codex_tokens = codex_tokens.clone();
                                        let sample = UsageLogSample {
                                            timestamp: chrono::Utc::now(),
                                            provider: result.name.clone(),
                                            account: result.account.clone(),
                                            source_label: result.source_label.clone(),
                                            plan: result.plan.clone(),
                                            model_name: result.model_name.clone(),
                                            session_percent: result.session_percent,
                                            weekly_percent: result.weekly_percent,
                                            model_percent: result.model_percent,
                                            codex_tokens,
                                        };
                                        result.usage_trends = append_and_summarize_usage(sample);
                                    }
                                    if id == ProviderId::Codex && result.error.is_none() {
                                        if let (Some(session_used), Some(weekly_used)) =
                                            (result.session_percent, result.weekly_percent)
                                        {
                                            let current = CodexUsageSample {
                                                session_used,
                                                weekly_used,
                                            };
                                            s.codex_window_ratio = update_codex_window_ratio(
                                                s.last_codex_sample,
                                                current,
                                                s.codex_window_ratio,
                                            );
                                            s.last_codex_sample = Some(current);
                                            result.codex_window_ratio = s.codex_window_ratio;
                                        }
                                    }
                                    s.providers[idx] = result;
                                }
                            }
                        })
                    })
                    .collect();

                for handle in handles {
                    let _ = handle.await;
                }
            });

            if let Ok(mut s) = state.lock() {
                s.last_refresh = Instant::now();
                s.is_refreshing = false;
                for provider in &mut s.providers {
                    provider.is_loading = false;
                }
            }
        });
    }

    /// Get an animated value that smoothly transitions to the target over 300ms.
    ///
    /// This helper provides consistent animation behavior for progress bar fills
    /// and other numeric value transitions.
    ///
    /// # Arguments
    /// * `ctx` - The egui context for animation state
    /// * `id` - A unique identifier for tracking this animation
    /// * `target` - The target value to animate towards
    ///
    /// # Returns
    /// The current animated value, which will smoothly approach the target
    #[allow(dead_code)]
    fn get_animated_value(ctx: &egui::Context, id: egui::Id, target: f32) -> f32 {
        ctx.animate_value_with_time(id, target, 0.3)
    }
}

fn work_area_rect(ctx: &egui::Context) -> Option<Rect> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::RECT as WinRect;
        use windows::Win32::UI::WindowsAndMessaging::{
            SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
        };

        let mut rect = WinRect::default();
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut rect as *mut WinRect).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .is_ok()
        };

        if ok {
            let pixels_per_point = ctx.pixels_per_point().max(0.1);
            return Some(Rect::from_min_max(
                egui::pos2(
                    rect.left as f32 / pixels_per_point,
                    rect.top as f32 / pixels_per_point,
                ),
                egui::pos2(
                    rect.right as f32 / pixels_per_point,
                    rect.bottom as f32 / pixels_per_point,
                ),
            ));
        }
    }

    ctx.input(|i| {
        i.viewport()
            .monitor_size
            .map(|size| Rect::from_min_size(egui::pos2(0.0, 0.0), size))
    })
}

fn create_provider(id: ProviderId) -> Box<dyn Provider> {
    match id {
        ProviderId::Claude => Box::new(ClaudeProvider::new()),
        ProviderId::Codex => Box::new(CodexProvider::new()),
        ProviderId::Cursor => Box::new(CursorProvider::new()),
        ProviderId::Gemini => Box::new(GeminiProvider::new()),
        ProviderId::Copilot => Box::new(CopilotProvider::new()),
        ProviderId::Antigravity => Box::new(AntigravityProvider::new()),
        ProviderId::Factory => Box::new(FactoryProvider::new()),
        ProviderId::Zai => Box::new(ZaiProvider::new()),
        ProviderId::Kiro => Box::new(KiroProvider::new()),
        ProviderId::VertexAI => Box::new(VertexAIProvider::new()),
        ProviderId::Augment => Box::new(AugmentProvider::new()),
        ProviderId::MiniMax => Box::new(MiniMaxProvider::new()),
        ProviderId::OpenCode => Box::new(OpenCodeProvider::new()),
        ProviderId::Kimi => Box::new(KimiProvider::new()),
        ProviderId::KimiK2 => Box::new(KimiK2Provider::new()),
        ProviderId::Amp => Box::new(AmpProvider::new()),
        ProviderId::Warp => Box::new(WarpProvider::new()),
        ProviderId::Ollama => Box::new(OllamaProvider::new()),
        ProviderId::OpenRouter => Box::new(OpenRouterProvider::new()),
        ProviderId::Synthetic => Box::new(SyntheticProvider::new()),
        ProviderId::JetBrains => Box::new(JetBrainsProvider::new()),
        ProviderId::Alibaba => Box::new(AlibabaProvider::new()),
    }
}

impl eframe::App for CodexBarApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Intercept window close: hide to tray instead of exiting
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        if self.pending_main_window_layout {
            self.layout_main_window(ctx, self.anchor_main_window_to_pointer);
        }

        // Check keyboard shortcuts without holding an immutable borrow of self
        // while triggering layout changes.
        let mut shortcut_triggered = false;
        if let Some(shortcut_mgr) = self.shortcut_manager.as_ref() {
            while shortcut_mgr.check_events() {
                shortcut_triggered = true;
            }
        }
        if shortcut_triggered {
            tracing::info!("Keyboard shortcut triggered - focusing window");
            restore_main_window();
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        // Process test input queue (debug builds only - for automated testing without moving real cursor)
        #[cfg(debug_assertions)]
        if let Ok(mut queue) = self.test_input_queue.lock() {
            let mut had_input = false;
            for input in queue.drain(..) {
                had_input = true;
                match input {
                    super::test_server::TestInput::Click { x, y } => {
                        let pos = egui::pos2(x, y);
                        ctx.input_mut(|i| {
                            // Move pointer to position first (required for hover detection)
                            i.events.push(egui::Event::PointerMoved(pos));
                            // Then click
                            i.events.push(egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed: true,
                                modifiers: egui::Modifiers::NONE,
                            });
                            i.events.push(egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Primary,
                                pressed: false,
                                modifiers: egui::Modifiers::NONE,
                            });
                        });
                        tracing::debug!("Injected test click at ({}, {})", x, y);
                    }
                    super::test_server::TestInput::DoubleClick { x, y } => {
                        let pos = egui::pos2(x, y);
                        ctx.input_mut(|i| {
                            // Move pointer to position first
                            i.events.push(egui::Event::PointerMoved(pos));
                            for _ in 0..2 {
                                i.events.push(egui::Event::PointerButton {
                                    pos,
                                    button: egui::PointerButton::Primary,
                                    pressed: true,
                                    modifiers: egui::Modifiers::NONE,
                                });
                                i.events.push(egui::Event::PointerButton {
                                    pos,
                                    button: egui::PointerButton::Primary,
                                    pressed: false,
                                    modifiers: egui::Modifiers::NONE,
                                });
                            }
                        });
                        tracing::debug!("Injected test double-click at ({}, {})", x, y);
                    }
                    super::test_server::TestInput::RightClick { x, y } => {
                        let pos = egui::pos2(x, y);
                        ctx.input_mut(|i| {
                            // Move pointer to position first
                            i.events.push(egui::Event::PointerMoved(pos));
                            i.events.push(egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Secondary,
                                pressed: true,
                                modifiers: egui::Modifiers::NONE,
                            });
                            i.events.push(egui::Event::PointerButton {
                                pos,
                                button: egui::PointerButton::Secondary,
                                pressed: false,
                                modifiers: egui::Modifiers::NONE,
                            });
                        });
                        tracing::debug!("Injected test right-click at ({}, {})", x, y);
                    }
                }
            }
            if had_input {
                ctx.request_repaint();
            }
        }

        // Auto-refresh check
        let should_refresh = {
            if self.settings.refresh_interval_secs == 0 {
                false
            } else if let Ok(state) = self.state.lock() {
                !state.is_refreshing
                    && state.last_refresh.elapsed()
                        > Duration::from_secs(self.settings.refresh_interval_secs)
            } else {
                false
            }
        };
        if should_refresh {
            self.refresh_providers();
        }

        // Get state
        let (
            providers,
            selected_idx,
            is_refreshing,
            loading_pattern,
            loading_phase,
            surprise_state,
            update_info,
            update_download_state,
            login_state,
        ) = {
            if let Ok(mut state) = self.state.lock() {
                if state.is_refreshing {
                    state.loading_phase += 0.05;
                    if state.loading_phase > 1.0 {
                        state.loading_phase -= 1.0;
                    }
                }

                let surprise = if self.settings.surprise_animations && !state.is_refreshing {
                    if let Some(anim) = state.surprise_animation {
                        state.surprise_frame += 1;
                        if state.surprise_frame >= anim.duration_frames() {
                            state.surprise_animation = None;
                            state.surprise_frame = 0;
                            state.next_surprise_time = Instant::now() + random_surprise_delay();
                            None
                        } else {
                            Some((anim, state.surprise_frame))
                        }
                    } else if Instant::now() >= state.next_surprise_time {
                        let anim = SurpriseAnimation::random();
                        state.surprise_animation = Some(anim);
                        state.surprise_frame = 0;
                        Some((anim, 0))
                    } else {
                        None
                    }
                } else {
                    None
                };

                let update = if state.update_dismissed {
                    None
                } else {
                    state.update_available.clone()
                };

                let update_download_state = state.update_state.clone();

                let login_state = (
                    state.login_provider.clone(),
                    state.login_phase,
                    state.login_message.clone(),
                );

                (
                    state.providers.clone(),
                    state.selected_provider_idx,
                    state.is_refreshing,
                    state.loading_pattern,
                    state.loading_phase,
                    surprise,
                    update,
                    update_download_state,
                    login_state,
                )
            } else {
                (
                    Vec::new(),
                    0,
                    false,
                    LoadingPattern::default(),
                    0.0,
                    None,
                    None,
                    UpdateState::Idle,
                    (None, LoginPhase::Idle, None),
                )
            }
        };

        let (_login_provider, login_phase, _login_message) = login_state;
        let is_logging_in = _login_provider.is_some() && login_phase != LoginPhase::Idle;

        ctx.request_repaint_after(
            if is_refreshing || surprise_state.is_some() || is_logging_in {
                Duration::from_millis(50)
            } else {
                Duration::from_millis(200)
            },
        );

        // Update tray icon
        if let Some(ref tray) = self.tray_manager {
            if is_refreshing {
                tray.show_loading(loading_pattern, loading_phase);
            } else if let Some((anim, frame)) = surprise_state {
                // Use first provider with data for surprise animation
                if let Some(provider) = providers.iter().find(|p| p.session_percent.is_some()) {
                    let session = provider.session_percent.unwrap_or(0.0);
                    let weekly = provider.weekly_percent.unwrap_or(session);
                    tray.show_surprise(anim, frame, session, weekly);
                }
            } else {
                // Respect menu_bar_display_mode setting
                // Use per-provider metric preferences from settings
                let provider_usages: Vec<ProviderUsage> = providers
                    .iter()
                    .filter(|p| p.session_percent.is_some())
                    .map(|p| {
                        // Get the metric preference for this provider
                        let metric_pref = crate::core::ProviderId::from_cli_name(&p.name)
                            .map(|id| self.settings.get_provider_metric(id))
                            .unwrap_or_default();
                        let preferred_percent = p.get_preferred_metric(metric_pref);
                        // For credits metric, convert from "remaining" to "used" for consistent tray behavior
                        // Credits are stored as remaining %, but tray expects used % for severity coloring
                        let used_percent =
                            if metric_pref == crate::settings::MetricPreference::Credits {
                                100.0 - preferred_percent // Convert remaining to used
                            } else {
                                preferred_percent // Already used %
                            };
                        // Weekly percent is always usage-based (not credits)
                        let weekly_percent = p.weekly_percent.unwrap_or(used_percent);
                        ProviderUsage {
                            name: p.display_name.clone(),
                            session_percent: used_percent, // Always use "used %" for tray severity
                            weekly_percent,
                        }
                    })
                    .collect();

                match self.settings.menu_bar_display_mode.as_str() {
                    "minimal" => {
                        // Minimal: show only the highest-usage provider's session bar
                        if let Some(p) = provider_usages.iter().max_by(|a, b| {
                            a.session_percent
                                .partial_cmp(&b.session_percent)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        }) {
                            tray.update_usage(p.session_percent, p.weekly_percent, &p.name);
                        }
                    }
                    "compact" => {
                        // Compact: merged icon but shorter tooltip (first provider only)
                        if let Some(p) = provider_usages.first() {
                            tray.update_usage(p.session_percent, p.weekly_percent, &p.name);
                        }
                    }
                    _ => {
                        // Detailed (default): show all providers merged
                        tray.update_merged(&provider_usages);
                    }
                }
            }

            let mut tray_actions = Vec::new();
            if let Some(ref action_rx) = self.tray_action_rx {
                while let Ok(action) = action_rx.try_recv() {
                    tray_actions.push(action);
                }
            }
            for action in tray_actions {
                match action {
                    TrayMenuAction::Quit => std::process::exit(0),
                    TrayMenuAction::Open => {
                        restore_main_window();
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    TrayMenuAction::Refresh => {
                        if !is_refreshing {
                            self.refresh_providers();
                            // Ensure animations advance even if window is hidden/minimized.
                            ctx.request_repaint();
                        }
                    }
                    TrayMenuAction::Settings => {
                        self.preferences_window.open();
                        // Move main window off-screen so only settings viewport is visible.
                        ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(
                            -10000.0, -10000.0,
                        )));
                    }
                    TrayMenuAction::CheckForUpdates => {
                        // Trigger update check in background
                        let state = Arc::clone(&self.state);
                        let update_channel = self.settings.update_channel;
                        let ignored_update_version = self.settings.ignored_update_version.clone();
                        std::thread::spawn(move || {
                            let rt = match tokio::runtime::Runtime::new() {
                                Ok(rt) => rt,
                                Err(e) => {
                                    tracing::error!("Failed to create runtime: {}", e);
                                    return;
                                }
                            };
                            rt.block_on(async {
                                if let Some(update) =
                                    updater::check_for_updates_with_channel(update_channel).await
                                {
                                    if ignored_update_version.as_deref()
                                        == Some(update.version.as_str())
                                    {
                                        if let Ok(mut s) = state.lock() {
                                            s.update_available = None;
                                            s.update_checked = true;
                                            s.update_state = UpdateState::Idle;
                                        }
                                        return;
                                    }

                                    if let Ok(mut s) = state.lock() {
                                        s.update_available = Some(update);
                                        s.update_checked = true;
                                        s.update_dismissed = false;
                                    }
                                } else if let Ok(mut s) = state.lock() {
                                    s.update_checked = true;
                                }
                            });
                        });
                    }
                    TrayMenuAction::ToggleProvider(provider_name) => {
                        // Toggle provider enabled state
                        if let Some(provider_id) = ProviderId::from_cli_name(&provider_name) {
                            self.settings.toggle_provider(provider_id);
                            if let Err(e) = self.settings.save() {
                                tracing::error!("Failed to save settings: {}", e);
                            }
                            // Refresh to update the UI with new provider list
                            self.refresh_providers();
                        }
                    }
                }
            }
        }

        // Apply refined style
        let mut style = (*ctx.style()).clone();
        style.visuals.window_fill = Theme::BG_PRIMARY;
        style.visuals.panel_fill = Theme::BG_PRIMARY;
        style.visuals.widgets.noninteractive.bg_fill = Theme::BG_SECONDARY;
        style.visuals.widgets.inactive.bg_fill = Theme::CARD_BG;
        style.visuals.widgets.hovered.bg_fill = Theme::CARD_BG_HOVER;
        style.visuals.widgets.active.bg_fill = Theme::ACCENT_PRIMARY;
        style.visuals.selection.bg_fill = Theme::selection_overlay();
        style.visuals.selection.stroke = Stroke::new(1.0, Theme::ACCENT_PRIMARY);
        ctx.set_style(style);

        // Handle keyboard shortcuts
        ctx.input(|i| {
            if i.modifiers.ctrl && i.key_pressed(egui::Key::Comma) {
                self.preferences_window.open();
            }
        });

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Theme::BG_PRIMARY).inner_margin(Spacing::SM))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                    // ════════════════════════════════════════════════════════════
                    // UPDATE BANNER
                    // ════════════════════════════════════════════════════════════
                    if let Some(ref update) = update_info {
                        egui::Frame::none()
                            .fill(Theme::ACCENT_PRIMARY)
                            .rounding(Rounding::same(Radius::LG))
                            .inner_margin(Spacing::MD)
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        // Icon based on state
                                        let icon = match &update_download_state {
                                            UpdateState::Ready(_) => "✓",
                                            UpdateState::Failed(_) => "⚠",
                                            UpdateState::Downloading(_) => "↓",
                                            _ => "🎉",
                                        };
                                        ui.label(RichText::new(icon).size(FontSize::MD).color(Color32::WHITE));
                                        ui.add_space(Spacing::XS);

                                        // Message based on state
                                        let message = match &update_download_state {
                                            UpdateState::Downloading(progress) => {
                                                format!("Downloading {} ({:.0}%)", update.version, progress * 100.0)
                                            }
                                            UpdateState::Ready(_) => {
                                                format!("Update {} ready to install", update.version)
                                            }
                                            UpdateState::Failed(e) => {
                                                format!("Update failed: {}", e)
                                            }
                                            _ => {
                                                format!("Update available: {}", update.version)
                                            }
                                        };
                                        ui.label(
                                            RichText::new(message)
                                                .size(FontSize::BASE)
                                                .color(Color32::WHITE)
                                                .strong(),
                                        );

                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            // Dismiss button
                                            if ui.add(
                                                egui::Button::new(RichText::new("✕").size(FontSize::SM).color(Color32::WHITE))
                                                    .fill(Color32::TRANSPARENT)
                                                    .stroke(Stroke::NONE)
                                            ).clicked() {
                                                self.settings.ignored_update_version =
                                                    Some(update.version.clone());
                                                if let Err(e) = self.settings.save() {
                                                    tracing::error!(
                                                        "Failed to save dismissed update version: {}",
                                                        e
                                                    );
                                                }
                                                if let Ok(mut s) = self.state.lock() {
                                                    s.update_dismissed = true;
                                                }
                                            }

                                            // Action button based on state
                                            match &update_download_state {
                                                UpdateState::Ready(path) => {
                                                    let installer_path = path.clone();
                                                    if ui.add(
                                                        egui::Button::new(RichText::new("Restart & Update").size(FontSize::SM).color(Theme::ACCENT_PRIMARY))
                                                            .fill(Color32::WHITE)
                                                            .rounding(Rounding::same(Radius::SM))
                                                    ).clicked() {
                                                        if let Err(e) = updater::apply_update(&installer_path) {
                                                            tracing::error!("Failed to apply update: {}", e);
                                                        }
                                                    }
                                                }
                                                UpdateState::Downloading(_) => {
                                                    // Show a small spinner or just the progress in the message
                                                    ui.spinner();
                                                }
                                                UpdateState::Failed(_) => {
                                                    let download_url = update.download_url.clone();
                                                    if ui.add(
                                                        egui::Button::new(RichText::new("Retry").size(FontSize::SM).color(Theme::ACCENT_PRIMARY))
                                                            .fill(Color32::WHITE)
                                                            .rounding(Rounding::same(Radius::SM))
                                                    ).clicked() {
                                                        // Retry download
                                                        let state = Arc::clone(&self.state);
                                                        let update_clone = update.clone();
                                                        std::thread::spawn(move || {
                                                            let rt = tokio::runtime::Runtime::new().unwrap();
                                                            rt.block_on(async {
                                                                if let Ok(mut s) = state.lock() {
                                                                    s.update_state = UpdateState::Downloading(0.0);
                                                                }
                                                                let (progress_tx, _) = tokio::sync::watch::channel(UpdateState::Available);
                                                                match updater::download_update(&update_clone, progress_tx).await {
                                                                    Ok(path) => {
                                                                        if let Ok(mut s) = state.lock() {
                                                                            s.update_state = UpdateState::Ready(path);
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        if let Ok(mut s) = state.lock() {
                                                                            s.update_state = UpdateState::Failed(e);
                                                                        }
                                                                    }
                                                                }
                                                            });
                                                        });
                                                    }
                                                    // Also show manual download link
                                                    if ui.add(
                                                        egui::Button::new(RichText::new("Download").size(FontSize::SM).color(Color32::WHITE))
                                                            .fill(Color32::TRANSPARENT)
                                                            .stroke(Stroke::new(1.0, Color32::WHITE))
                                                            .rounding(Rounding::same(Radius::SM))
                                                    ).clicked() {
                                                        let _ = open::that(&download_url);
                                                    }
                                                }
                                                _ => {
                                                    // Available or Idle - show download button
                                                    let update_clone = update.clone();
                                                    if ui.add(
                                                        egui::Button::new(RichText::new("Download").size(FontSize::SM).color(Theme::ACCENT_PRIMARY))
                                                            .fill(Color32::WHITE)
                                                            .rounding(Rounding::same(Radius::SM))
                                                    ).clicked() {
                                                        // Start download
                                                        let state = Arc::clone(&self.state);
                                                        std::thread::spawn(move || {
                                                            let rt = tokio::runtime::Runtime::new().unwrap();
                                                            rt.block_on(async {
                                                                if let Ok(mut s) = state.lock() {
                                                                    s.update_state = UpdateState::Downloading(0.0);
                                                                }
                                                                let (progress_tx, _) = tokio::sync::watch::channel(UpdateState::Available);
                                                                match updater::download_update(&update_clone, progress_tx).await {
                                                                    Ok(path) => {
                                                                        if let Ok(mut s) = state.lock() {
                                                                            s.update_state = UpdateState::Ready(path);
                                                                        }
                                                                    }
                                                                    Err(e) => {
                                                                        if let Ok(mut s) = state.lock() {
                                                                            s.update_state = UpdateState::Failed(e);
                                                                        }
                                                                    }
                                                                }
                                                            });
                                                        });
                                                    }
                                                }
                                            }
                                        });
                                    });

                                    // Show download progress bar when downloading
                                    if let UpdateState::Downloading(progress) = &update_download_state {
                                        ui.add_space(Spacing::XS);
                                        let bar_width = ui.available_width();
                                        let bar_height = 4.0;
                                        let (rect, _) = ui.allocate_exact_size(Vec2::new(bar_width, bar_height), egui::Sense::hover());

                                        // Track (semi-transparent white)
                                        ui.painter().rect_filled(rect, Rounding::same(2.0), Color32::from_rgba_unmultiplied(255, 255, 255, 80));

                                        // Fill (solid white)
                                        let fill_w = rect.width() * progress.clamp(0.0, 1.0);
                                        if fill_w > 0.0 {
                                            let fill_rect = Rect::from_min_size(rect.min, Vec2::new(fill_w, bar_height));
                                            ui.painter().rect_filled(fill_rect, Rounding::same(2.0), Color32::WHITE);
                                        }
                                    }
                                });
                            });
                        ui.add_space(Spacing::MD);
                    }

                    if !providers.is_empty() {
                        let visible_providers: Vec<(usize, &ProviderData)> = providers.iter()
                            .enumerate()
                            .filter(|(_, p)| p.session_percent.is_some() || p.error.is_some() || p.is_loading)
                            .collect();

                        if !visible_providers.is_empty() {
                            let mut manual_refresh_requested = false;
                            let mut account_switch_provider: Option<String> = None;
                            let show_credits = self.settings.show_credits_extra_usage;
                            let show_as_used = self.settings.show_as_used;
                            let hide_personal_info = self.settings.hide_personal_info;

                            section_header_inline(ui, "Dashboard");
                            ui.add_space(4.0);
                            for (original_idx, provider) in &visible_providers {
                                let is_selected = *original_idx == selected_idx;
                                let clicked = draw_overview_provider_row(
                                    ui,
                                    provider,
                                    &mut self.icon_cache,
                                    show_as_used,
                                    is_selected,
                                );
                                if clicked {
                                    if let Ok(mut state) = self.state.lock() {
                                        state.selected_provider_idx = *original_idx;
                                    }
                                }
                                ui.add_space(4.0);
                            }

                            ui.add_space(4.0);
                            draw_horizontal_separator(ui, 0.0);
                            ui.add_space(4.0);
                            if let Some((_, selected_provider)) = visible_providers.iter().find(|(idx, _)| *idx == selected_idx) {
                                let (refresh, switch) = draw_provider_detail_card(
                                    ui,
                                    selected_provider,
                                    &mut self.icon_cache,
                                    show_credits,
                                    show_as_used,
                                    hide_personal_info,
                                );
                                manual_refresh_requested = refresh;
                                account_switch_provider = switch;
                            } else if let Some((_, first_provider)) = visible_providers.first() {
                                // Fallback to first if selected isn't visible
                                let (refresh, switch) = draw_provider_detail_card(
                                    ui,
                                    first_provider,
                                    &mut self.icon_cache,
                                    show_credits,
                                    show_as_used,
                                    hide_personal_info,
                                );
                                manual_refresh_requested = refresh;
                                account_switch_provider = switch;
                            }

                            // Trigger manual refresh if requested
                            if manual_refresh_requested && !is_refreshing {
                                self.refresh_providers();
                            }

                            // Handle account switch request - open preferences to Providers tab with provider selected
                            if let Some(provider_name) = account_switch_provider {
                                if let Some(provider_id) = ProviderId::from_cli_name(&provider_name) {
                                    self.preferences_window.active_tab = super::preferences::PreferencesTab::Providers;
                                    self.preferences_window.selected_provider = Some(provider_id);
                                    self.preferences_window.open();
                                }
                            }
                        } else if is_refreshing {
                            egui::Frame::none()
                                .fill(Theme::CARD_BG)
                                .rounding(Rounding::same(Radius::LG))
                                .inner_margin(Spacing::XXL)
                                .stroke(Stroke::new(1.0, Theme::CARD_BORDER))
                                .show(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        ui.spinner();
                                        ui.add_space(Spacing::SM);
                                        ui.label(
                                            RichText::new("Loading providers...")
                                                .size(FontSize::BASE)
                                                .color(Theme::TEXT_MUTED),
                                        );
                                    });
                                });
                        } else {
                            egui::Frame::none()
                                .fill(Theme::CARD_BG)
                                .rounding(Rounding::same(Radius::LG))
                                .inner_margin(Spacing::XXL)
                                .stroke(Stroke::new(1.0, Theme::CARD_BORDER))
                                .show(ui, |ui| {
                                    ui.vertical_centered(|ui| {
                                        ui.label(
                                            RichText::new("No provider data available.")
                                                .size(FontSize::BASE)
                                                .color(Theme::TEXT_MUTED),
                                        );
                                    });
                                });
                        }
                    } else {
                        let has_enabled_providers = !self.settings.get_enabled_provider_ids().is_empty();
                        egui::Frame::none()
                            .fill(Theme::CARD_BG)
                            .rounding(Rounding::same(Radius::LG))
                            .inner_margin(Spacing::XXL)
                            .stroke(Stroke::new(1.0, Theme::CARD_BORDER))
                            .show(ui, |ui| {
                                ui.vertical_centered(|ui| {
                                    if has_enabled_providers {
                                        ui.spinner();
                                        ui.add_space(Spacing::SM);
                                        ui.label(
                                            RichText::new("Loading providers...")
                                                .size(FontSize::BASE)
                                                .color(Theme::TEXT_MUTED),
                                        );
                                    } else {
                                        ui.label(
                                            RichText::new("No providers selected.")
                                                .size(FontSize::BASE)
                                                .color(Theme::TEXT_MUTED),
                                        );
                                        ui.add_space(Spacing::SM);
                                        if ui.button("Open Provider Settings").clicked() {
                                            self.preferences_window.active_tab = super::preferences::PreferencesTab::Providers;
                                            self.preferences_window.open();
                                        }
                                    }
                                });
                            });
                    }

                    ui.add_space(4.0);

                    // ════════════════════════════════════════════════════════════
                    // BOTTOM MENU - macOS style vertical text items
                    // ════════════════════════════════════════════════════════════
                    draw_horizontal_separator(ui, 0.0);
                    ui.add_space(4.0);

                    if draw_text_menu_item(ui, "Settings...") {
                        self.preferences_window.open();
                    }
                    if draw_text_menu_item(ui, "About CodexBar") {
                        self.preferences_window.active_tab = super::preferences::PreferencesTab::About;
                        self.preferences_window.open();
                    }
                    if draw_text_menu_item(ui, "Quit") {
                        std::process::exit(0);
                    }
                }); // end ScrollArea
            });

        // Show preferences window
        self.preferences_window.show(ctx);

        let mut refresh_requested = self.preferences_window.take_refresh_requested();
        let previous_enabled_provider_ids = self.settings.get_enabled_provider_ids();

        // Atomically consume settings changes so the flag is cleared in both
        // PreferencesWindow and the shared viewport state in one shot.
        if let Some(new_settings) = self.preferences_window.take_settings_if_changed() {
            self.settings = new_settings;
            if let Err(e) = self.settings.save() {
                tracing::error!("Failed to save settings: {}", e);
            }
            if previous_enabled_provider_ids != self.settings.get_enabled_provider_ids() {
                refresh_requested = true;
            }
        }

        // Check if preferences window requested a provider refresh
        if refresh_requested {
            self.refresh_providers();
        }

        // Reload preferences snapshot when refresh completes (is_refreshing transitions to false)
        if self.preferences_window.is_open {
            let is_refreshing = self.state.lock().map(|s| s.is_refreshing).unwrap_or(false);
            if self.was_refreshing && !is_refreshing {
                // Refresh just completed - reload the snapshot
                self.preferences_window.reload_snapshot();
            }
            self.was_refreshing = is_refreshing;
        }
    }
}

/// Compact row for the Overview tab: icon, name, and usage bar
fn draw_overview_provider_row(
    ui: &mut egui::Ui,
    provider: &ProviderData,
    _icon_cache: &mut ProviderIconCache,
    show_as_used: bool,
    is_selected: bool,
) -> bool {
    let brand_color = provider_color(&provider.name);
    let primary_percent = provider
        .session_percent
        .map(|percent| usage_display_percent(percent, show_as_used));
    let primary_color = primary_percent
        .map(metric_severity_color)
        .unwrap_or(Theme::TEXT_PRIMARY);

    let inner = egui::Frame::none()
        .fill(if is_selected {
            Theme::CARD_BG_HOVER
        } else {
            Theme::CARD_BG
        })
        .rounding(Rounding::same(Radius::SM))
        .inner_margin(egui::Margin::symmetric(12.0, 8.0))
        .stroke(Stroke::new(
            if is_selected { 1.0 } else { 0.5 },
            if is_selected {
                brand_color
            } else {
                Theme::CARD_BORDER
            },
        ))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let icon_char = provider_icon(&provider.name);
                    ui.label(
                        RichText::new(icon_char)
                            .size(FontSize::BASE)
                            .color(brand_color),
                    );

                    ui.add_space(4.0);

                    ui.label(
                        RichText::new(&provider.display_name)
                            .size(FontSize::SM)
                            .color(Theme::TEXT_PRIMARY)
                            .strong(),
                    );

                    if let Some(source) = provider.source_label.as_ref() {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(source.to_uppercase())
                                .size(FontSize::XS)
                                .color(Theme::TEXT_MUTED),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if provider.is_loading {
                            ui.spinner();
                        } else if let Some(error) = &provider.error {
                            let short = if error.len() > 30 {
                                format!("{}…", &error[..29])
                            } else {
                                error.clone()
                            };
                            ui.label(RichText::new(short).size(FontSize::XS).color(Theme::RED));
                        } else if let Some(percent) = primary_percent {
                            ui.label(
                                RichText::new(usage_display_label(percent, show_as_used))
                                    .size(FontSize::XS)
                                    .color(primary_color)
                                    .strong(),
                            );
                        }
                    });
                });

                ui.add_space(6.0);

                draw_provider_dashboard_bar(ui, provider, show_as_used, brand_color);

                ui.add_space(5.0);

                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
                    draw_provider_dashboard_stats(ui, provider, show_as_used);
                });
            });
        });
    let response = ui.interact(
        inner.response.rect,
        ui.id().with(("provider_row", provider.name.as_str())),
        egui::Sense::click(),
    );
    response.clicked()
}

fn draw_dashboard_stat(
    ui: &mut egui::Ui,
    label: &str,
    value: String,
    detail: Option<&str>,
    value_color: Color32,
) {
    draw_dashboard_stat_with_detail_color(ui, label, value, detail, value_color, None);
}

fn draw_dashboard_stat_with_detail_color(
    ui: &mut egui::Ui,
    label: &str,
    value: String,
    detail: Option<&str>,
    value_color: Color32,
    detail_color: Option<Color32>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("{}:", label))
                .size(FontSize::XS)
                .color(Theme::TEXT_SECONDARY),
        );
        ui.label(
            RichText::new(value)
                .size(FontSize::XS)
                .color(value_color)
                .strong(),
        );
        if let Some(detail) = detail {
            ui.label(
                RichText::new(format!("· {}", detail))
                    .size(FontSize::XS)
                    .color(detail_color.unwrap_or(Theme::TEXT_SECONDARY)),
            );
        }
    });
}

fn draw_provider_dashboard_bar(
    ui: &mut egui::Ui,
    provider: &ProviderData,
    show_as_used: bool,
    _brand_color: Color32,
) {
    match provider.name.as_str() {
        "claude" => {
            let bar_width = ui.available_width();
            let bar_height = 5.0;
            let bar_gap = 3.0;
            let total_height = (bar_height * 2.0) + bar_gap;
            let (stack_rect, _) =
                ui.allocate_exact_size(Vec2::new(bar_width, total_height), egui::Sense::hover());
            let session_rect =
                Rect::from_min_size(stack_rect.min, Vec2::new(bar_width, bar_height));
            let weekly_rect = Rect::from_min_size(
                egui::pos2(stack_rect.min.x, stack_rect.min.y + bar_height + bar_gap),
                Vec2::new(bar_width, bar_height),
            );

            for rect in [session_rect, weekly_rect] {
                ui.painter().rect_filled(
                    rect,
                    Rounding::same(bar_height / 2.0),
                    Theme::progress_track(),
                );
            }

            if let Some(session_percent) = provider.session_percent {
                let display_percent = usage_display_percent(session_percent, show_as_used);
                fill_bar(
                    ui,
                    session_rect,
                    display_percent,
                    metric_severity_color(display_percent),
                );
                draw_pace_marker(
                    ui,
                    session_rect,
                    display_percent,
                    provider.session_pace_percent,
                    show_as_used,
                );
            }
            if let Some(weekly_percent) = provider.weekly_percent {
                let display_percent = usage_display_percent(weekly_percent, show_as_used);
                fill_bar(
                    ui,
                    weekly_rect,
                    display_percent,
                    tinted(metric_severity_color(display_percent), 210),
                );
                draw_pace_marker(
                    ui,
                    weekly_rect,
                    display_percent,
                    provider.weekly_pace_percent,
                    show_as_used,
                );
            }
        }
        "codex" => {
            let bar_width = ui.available_width();
            let bar_height = 5.0;
            let bar_gap = 3.0;
            let total_height = (bar_height * 2.0) + bar_gap;
            let (stack_rect, _) =
                ui.allocate_exact_size(Vec2::new(bar_width, total_height), egui::Sense::hover());

            let session_rect =
                Rect::from_min_size(stack_rect.min, Vec2::new(bar_width, bar_height));
            let weekly_rect = Rect::from_min_size(
                egui::pos2(stack_rect.min.x, stack_rect.min.y + bar_height + bar_gap),
                Vec2::new(bar_width, bar_height),
            );

            ui.painter().rect_filled(
                session_rect,
                Rounding::same(bar_height / 2.0),
                Theme::progress_track(),
            );
            ui.painter().rect_filled(
                weekly_rect,
                Rounding::same(bar_height / 2.0),
                Theme::progress_track(),
            );

            if let Some(session_percent) = provider.session_percent {
                let display_percent = usage_display_percent(session_percent, show_as_used);
                fill_bar(
                    ui,
                    session_rect,
                    display_percent,
                    metric_severity_color(display_percent),
                );
                draw_pace_marker(
                    ui,
                    session_rect,
                    display_percent,
                    provider.session_pace_percent,
                    show_as_used,
                );
            }
            if let Some(weekly_percent) = provider.weekly_percent {
                let display_percent = usage_display_percent(weekly_percent, show_as_used);
                fill_bar(
                    ui,
                    weekly_rect,
                    display_percent,
                    tinted(metric_severity_color(display_percent), 210),
                );
                draw_pace_marker(
                    ui,
                    weekly_rect,
                    display_percent,
                    provider.weekly_pace_percent,
                    show_as_used,
                );
            }
        }
        "cursor" => {
            let bar_width = ui.available_width();
            let bar_height = 5.0;
            let bar_gap = 3.0;
            let total_height = (bar_height * 2.0) + bar_gap;
            let (stack_rect, _) =
                ui.allocate_exact_size(Vec2::new(bar_width, total_height), egui::Sense::hover());

            let auto_rect = Rect::from_min_size(stack_rect.min, Vec2::new(bar_width, bar_height));
            let api_rect = Rect::from_min_size(
                egui::pos2(stack_rect.min.x, stack_rect.min.y + bar_height + bar_gap),
                Vec2::new(bar_width, bar_height),
            );

            ui.painter().rect_filled(
                auto_rect,
                Rounding::same(bar_height / 2.0),
                Theme::progress_track(),
            );
            ui.painter().rect_filled(
                api_rect,
                Rounding::same(bar_height / 2.0),
                Theme::progress_track(),
            );

            if let Some(auto_percent) = provider.weekly_percent {
                let display_percent = usage_display_percent(auto_percent, show_as_used);
                fill_bar(
                    ui,
                    auto_rect,
                    display_percent,
                    cursor_metric_color(
                        display_percent,
                        provider.weekly_pace_percent,
                        provider.weekly_pace_lasts_to_reset,
                    ),
                );
                draw_pace_marker(
                    ui,
                    auto_rect,
                    display_percent,
                    provider.weekly_pace_percent,
                    show_as_used,
                );
            }
            if let Some(api_percent) = provider.model_percent {
                let display_percent = usage_display_percent(api_percent, show_as_used);
                fill_bar(
                    ui,
                    api_rect,
                    display_percent,
                    tinted(
                        cursor_metric_color(
                            display_percent,
                            provider.model_pace_percent,
                            provider.model_pace_lasts_to_reset,
                        ),
                        210,
                    ),
                );
                draw_pace_marker(
                    ui,
                    api_rect,
                    display_percent,
                    provider.model_pace_percent,
                    show_as_used,
                );
            }
        }
        "gemini" | "antigravity" => {
            let bar_width = ui.available_width();
            let bar_height = 4.0;
            let bar_gap = 3.0;
            let total_height = (bar_height * 3.0) + (bar_gap * 2.0);
            let (stack_rect, _) =
                ui.allocate_exact_size(Vec2::new(bar_width, total_height), egui::Sense::hover());

            let pro_rect = Rect::from_min_size(stack_rect.min, Vec2::new(bar_width, bar_height));
            let flash_rect = Rect::from_min_size(
                egui::pos2(stack_rect.min.x, stack_rect.min.y + bar_height + bar_gap),
                Vec2::new(bar_width, bar_height),
            );
            let other_rect = Rect::from_min_size(
                egui::pos2(
                    stack_rect.min.x,
                    stack_rect.min.y + (bar_height + bar_gap) * 2.0,
                ),
                Vec2::new(bar_width, bar_height),
            );

            for rect in [pro_rect, flash_rect, other_rect] {
                ui.painter().rect_filled(
                    rect,
                    Rounding::same(bar_height / 2.0),
                    Theme::progress_track(),
                );
            }

            if let Some(pro_percent) = provider.session_percent {
                let display_percent = usage_display_percent(pro_percent, show_as_used);
                fill_bar(
                    ui,
                    pro_rect,
                    display_percent,
                    metric_severity_color(display_percent),
                );
            }
            if let Some(flash_percent) = provider.weekly_percent {
                let display_percent = usage_display_percent(flash_percent, show_as_used);
                fill_bar(
                    ui,
                    flash_rect,
                    display_percent,
                    tinted(metric_severity_color(display_percent), 210),
                );
            }
            if let Some(other_percent) = provider.model_percent {
                let display_percent = usage_display_percent(other_percent, show_as_used);
                fill_bar(
                    ui,
                    other_rect,
                    display_percent,
                    tinted(metric_severity_color(display_percent), 190),
                );
            }
        }
        _ => {
            let bar_width = ui.available_width();
            let bar_height = 6.0;
            let (bar_rect, _) =
                ui.allocate_exact_size(Vec2::new(bar_width, bar_height), egui::Sense::hover());

            ui.painter()
                .rect_filled(bar_rect, Rounding::same(3.0), Theme::progress_track());

            if let Some(primary_percent) = provider.session_percent {
                let display_percent = usage_display_percent(primary_percent, show_as_used);
                fill_bar(
                    ui,
                    bar_rect,
                    display_percent,
                    metric_severity_color(display_percent),
                );
            }
        }
    }
}

fn fill_bar(ui: &egui::Ui, rect: Rect, percent: f64, color: Color32) {
    let fill_width = rect.width() * (percent as f32 / 100.0).clamp(0.0, 1.0);
    if fill_width > 0.0 {
        let fill_rect = Rect::from_min_size(rect.min, Vec2::new(fill_width, rect.height()));
        ui.painter()
            .rect_filled(fill_rect, Rounding::same(rect.height() / 2.0), color);
    }
}

fn draw_pace_marker(
    ui: &egui::Ui,
    rect: Rect,
    display_percent: f64,
    pace_percent: Option<f64>,
    show_as_used: bool,
) {
    let Some(pace_percent) = pace_percent else {
        return;
    };

    let display_pace_percent = if show_as_used {
        pace_percent
    } else {
        -pace_percent
    };
    let expected_position = (display_percent - display_pace_percent).clamp(0.0, 100.0);
    let marker_x = rect.min.x + rect.width() * (expected_position as f32 / 100.0);
    let marker_width = 2.0;
    let marker_rect = Rect::from_min_size(
        egui::pos2(marker_x - marker_width / 2.0, rect.min.y - 1.0),
        Vec2::new(marker_width, rect.height() + 2.0),
    );

    ui.painter().rect_filled(
        marker_rect,
        Rounding::same(1.0),
        Color32::from_rgba_unmultiplied(255, 255, 255, 190),
    );
}

fn draw_provider_dashboard_stats(ui: &mut egui::Ui, provider: &ProviderData, show_as_used: bool) {
    match provider.name.as_str() {
        "claude" => {
            let primary_label = provider.source_label.as_deref().unwrap_or("Primary");
            let session_display = provider
                .session_percent
                .map(|percent| usage_display_percent(percent, show_as_used));
            let session_reset_color = session_display.and_then(|display_percent| {
                pace_status(
                    display_percent,
                    provider.session_pace_percent,
                    provider.session_pace_lasts_to_reset,
                )
                .map(|(_, color)| color)
            });
            draw_dashboard_stat_with_detail_color(
                ui,
                &primary_label.to_uppercase(),
                provider
                    .session_percent
                    .map(|percent| {
                        usage_display_label(
                            usage_display_percent(percent, show_as_used),
                            show_as_used,
                        )
                    })
                    .unwrap_or_else(|| "No data".to_string()),
                provider.session_reset.as_deref(),
                provider
                    .session_percent
                    .map(|percent| {
                        metric_severity_color(usage_display_percent(percent, show_as_used))
                    })
                    .unwrap_or(Theme::TEXT_PRIMARY),
                session_reset_color,
            );
            if let Some(weekly_percent) = provider.weekly_percent {
                let weekly_display = usage_display_percent(weekly_percent, show_as_used);
                let weekly_reset_color = pace_status(
                    weekly_display,
                    provider.weekly_pace_percent,
                    provider.weekly_pace_lasts_to_reset,
                )
                .map(|(_, color)| color);
                draw_dashboard_stat_with_detail_color(
                    ui,
                    "Woche",
                    usage_display_label(weekly_display, show_as_used),
                    provider.weekly_reset.as_deref(),
                    metric_severity_color(weekly_display),
                    weekly_reset_color,
                );
            }
        }
        "codex" => {
            let session_display = provider
                .session_percent
                .map(|percent| usage_display_percent(percent, show_as_used));
            let session_reset_color = session_display.and_then(|display_percent| {
                pace_status(
                    display_percent,
                    provider.session_pace_percent,
                    provider.session_pace_lasts_to_reset,
                )
                .map(|(_, color)| color)
            });
            draw_dashboard_stat_with_detail_color(
                ui,
                "5h",
                provider
                    .session_percent
                    .map(|percent| {
                        usage_display_label(
                            usage_display_percent(percent, show_as_used),
                            show_as_used,
                        )
                    })
                    .unwrap_or_else(|| "No data".to_string()),
                provider.session_reset.as_deref(),
                provider
                    .session_percent
                    .map(|percent| {
                        metric_severity_color(usage_display_percent(percent, show_as_used))
                    })
                    .unwrap_or(Theme::TEXT_PRIMARY),
                session_reset_color,
            );
            if let Some(weekly_percent) = provider.weekly_percent {
                let weekly_display = usage_display_percent(weekly_percent, show_as_used);
                let weekly_reset_color = pace_status(
                    weekly_display,
                    provider.weekly_pace_percent,
                    provider.weekly_pace_lasts_to_reset,
                )
                .map(|(_, color)| color);
                draw_dashboard_stat_with_detail_color(
                    ui,
                    "Week",
                    usage_display_label(weekly_display, show_as_used),
                    provider.weekly_reset.as_deref(),
                    metric_severity_color(weekly_display),
                    weekly_reset_color,
                );
            }
        }
        "cursor" => {
            if let Some(auto_percent) = provider.weekly_percent {
                let auto_display = usage_display_percent(auto_percent, show_as_used);
                let auto_reset_color =
                    cursor_reset_time_color(auto_percent, provider.weekly_pace_percent);
                draw_dashboard_stat_with_detail_color(
                    ui,
                    "Auto+Composer",
                    usage_display_label(auto_display, show_as_used),
                    provider.weekly_reset.as_deref(),
                    cursor_metric_color(
                        auto_display,
                        provider.weekly_pace_percent,
                        provider.weekly_pace_lasts_to_reset,
                    ),
                    auto_reset_color,
                );
            }
            if let Some(api_percent) = provider.model_percent {
                let api_display = usage_display_percent(api_percent, show_as_used);
                let api_reset_color =
                    cursor_reset_time_color(api_percent, provider.model_pace_percent);
                draw_dashboard_stat_with_detail_color(
                    ui,
                    "API",
                    usage_display_label(api_display, show_as_used),
                    provider.model_reset.as_deref(),
                    cursor_metric_color(
                        api_display,
                        provider.model_pace_percent,
                        provider.model_pace_lasts_to_reset,
                    ),
                    api_reset_color,
                );
            }
            let total_reset_color = provider.session_percent.and_then(|percent| {
                cursor_reset_time_color(percent, provider.session_pace_percent)
            });
            draw_dashboard_stat_with_detail_color(
                ui,
                "Total",
                provider
                    .session_percent
                    .map(|percent| {
                        usage_display_label(
                            usage_display_percent(percent, show_as_used),
                            show_as_used,
                        )
                    })
                    .unwrap_or_else(|| "No data".to_string()),
                provider.session_reset.as_deref(),
                provider
                    .session_percent
                    .map(|percent| {
                        let display_percent = usage_display_percent(percent, show_as_used);
                        cursor_metric_color(
                            display_percent,
                            provider.session_pace_percent,
                            provider.session_pace_lasts_to_reset,
                        )
                    })
                    .unwrap_or(Theme::TEXT_PRIMARY),
                total_reset_color,
            );
        }
        "gemini" => {
            draw_dashboard_stat(
                ui,
                "Pro",
                provider
                    .session_percent
                    .map(|percent| {
                        usage_display_label(
                            usage_display_percent(percent, show_as_used),
                            show_as_used,
                        )
                    })
                    .unwrap_or_else(|| "No data".to_string()),
                provider.session_reset.as_deref(),
                provider
                    .session_percent
                    .map(|percent| {
                        metric_severity_color(usage_display_percent(percent, show_as_used))
                    })
                    .unwrap_or(Theme::TEXT_PRIMARY),
            );
            if let Some(flash_percent) = provider.weekly_percent {
                draw_dashboard_stat(
                    ui,
                    "Flash",
                    usage_display_label(
                        usage_display_percent(flash_percent, show_as_used),
                        show_as_used,
                    ),
                    provider.weekly_reset.as_deref(),
                    metric_severity_color(usage_display_percent(flash_percent, show_as_used)),
                );
            }
            if let Some(other_percent) = provider.model_percent {
                draw_dashboard_stat(
                    ui,
                    "Other",
                    usage_display_label(
                        usage_display_percent(other_percent, show_as_used),
                        show_as_used,
                    ),
                    provider.model_name.as_deref(),
                    metric_severity_color(usage_display_percent(other_percent, show_as_used)),
                );
            }
        }
        "antigravity" => {
            draw_dashboard_stat(
                ui,
                "Claude",
                provider
                    .session_percent
                    .map(|percent| {
                        usage_display_label(
                            usage_display_percent(percent, show_as_used),
                            show_as_used,
                        )
                    })
                    .unwrap_or_else(|| "No data".to_string()),
                provider.session_reset.as_deref(),
                provider
                    .session_percent
                    .map(|percent| {
                        metric_severity_color(usage_display_percent(percent, show_as_used))
                    })
                    .unwrap_or(Theme::TEXT_PRIMARY),
            );
            if let Some(pro_percent) = provider.weekly_percent {
                draw_dashboard_stat(
                    ui,
                    "Gemini Pro",
                    usage_display_label(
                        usage_display_percent(pro_percent, show_as_used),
                        show_as_used,
                    ),
                    provider.weekly_reset.as_deref(),
                    metric_severity_color(usage_display_percent(pro_percent, show_as_used)),
                );
            }
            if let Some(flash_percent) = provider.model_percent {
                draw_dashboard_stat(
                    ui,
                    "Gemini Flash",
                    usage_display_label(
                        usage_display_percent(flash_percent, show_as_used),
                        show_as_used,
                    ),
                    None,
                    metric_severity_color(usage_display_percent(flash_percent, show_as_used)),
                );
            }
        }
        _ => {
            draw_dashboard_stat(
                ui,
                "Primary",
                provider
                    .session_percent
                    .map(|percent| {
                        usage_display_label(
                            usage_display_percent(percent, show_as_used),
                            show_as_used,
                        )
                    })
                    .unwrap_or_else(|| "No data".to_string()),
                provider.session_reset.as_deref(),
                provider
                    .session_percent
                    .map(|percent| {
                        metric_severity_color(usage_display_percent(percent, show_as_used))
                    })
                    .unwrap_or(Theme::TEXT_PRIMARY),
            );
            if let Some(secondary_percent) = provider.weekly_percent {
                draw_dashboard_stat(
                    ui,
                    "Secondary",
                    usage_display_label(
                        usage_display_percent(secondary_percent, show_as_used),
                        show_as_used,
                    ),
                    provider.weekly_reset.as_deref(),
                    metric_severity_color(usage_display_percent(secondary_percent, show_as_used)),
                );
            }
            if let Some(model_percent) = provider.model_percent {
                draw_dashboard_stat(
                    ui,
                    "Model",
                    usage_display_label(
                        usage_display_percent(model_percent, show_as_used),
                        show_as_used,
                    ),
                    provider.model_name.as_deref(),
                    metric_severity_color(usage_display_percent(model_percent, show_as_used)),
                );
            }
        }
    }

    if let Some(cost) = provider.cost_used.as_ref() {
        draw_dashboard_stat(ui, "Spend", cost.clone(), None, Theme::TEXT_PRIMARY);
    }

    if let Some(plan) = provider.plan.as_ref() {
        draw_dashboard_stat(ui, "Plan", plan.clone(), None, Theme::TEXT_PRIMARY);
    }
}

/// Draw a provider detail card - macOS UsageMenuCardView style
/// Structure: Header -> Divider -> Metrics (Session, Weekly, Model) -> Credits -> Cost
/// Returns (refresh_requested, account_switch_provider_name)
fn draw_provider_detail_card(
    ui: &mut egui::Ui,
    provider: &ProviderData,
    _icon_cache: &mut ProviderIconCache,
    show_credits_extra: bool,
    show_as_used: bool,
    hide_personal_info: bool,
) -> (bool, Option<String>) {
    let mut refresh_requested = false;
    let mut account_switch_requested: Option<String> = None;
    let brand_color = provider_color(&provider.name);
    let content_width = ui.available_width() - 32.0; // 16px padding each side

    // Main VStack with spacing: 4 (compact)
    ui.vertical(|ui| {
        ui.add_space(1.0); // Top padding

        // ═══════════════════════════════════════════════════════════════════
        // HEADER SECTION - UsageMenuCardHeaderView
        // ═══════════════════════════════════════════════════════════════════
        ui.horizontal(|ui| {
            ui.add_space(16.0); // Left padding

            ui.vertical(|ui| {
                // Row 1: Provider name (left) | Email (right)
                ui.horizontal(|ui| {
                    // Provider name - .headline, .semibold
                    ui.label(
                        RichText::new(&provider.display_name)
                            .size(FontSize::BASE) // Slightly smaller
                            .color(Theme::TEXT_PRIMARY)
                            .strong(),
                    );

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(16.0); // Right padding
                        if provider.is_loading {
                            ui.spinner();
                            ui.add_space(6.0);
                        }
                        // Email - .subheadline, secondary color (redacted if privacy mode enabled)
                        if let Some(account) = &provider.account {
                            let display_account = PersonalInfoRedactor::redact_email(
                                Some(account.as_str()),
                                hide_personal_info,
                            );
                            if !display_account.is_empty() {
                                ui.label(
                                    RichText::new(&display_account)
                                        .size(FontSize::XS) // Smaller
                                        .color(Theme::TEXT_SECONDARY),
                                );
                            }
                        }
                    });
                });

                ui.add_space(1.0); // VStack spacing in header

                // Row 2: Subtitle/Error (left) | Plan (right)
                ui.horizontal(|ui| {
                    // Subtitle - .footnote
                    if let Some(error) = &provider.error {
                        ui.label(
                            RichText::new(error)
                                .size(FontSize::XS) // 11px footnote
                                .color(Theme::RED),
                        );
                    } else {
                        ui.label(
                            RichText::new("Updated just now")
                                .size(FontSize::XS)
                                .color(Theme::TEXT_SECONDARY),
                        );
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(16.0); // Right padding
                        // Plan badge - .footnote, secondary
                        if let Some(plan) = &provider.plan {
                            ui.label(
                                RichText::new(plan)
                                    .size(FontSize::XS)
                                    .color(Theme::TEXT_SECONDARY),
                            );
                        }
                    });
                });

                // Row 3: Status description (if non-Operational)
                if provider.status_level != StatusLevel::Operational
                    && provider.status_level != StatusLevel::Unknown
                {
                    if let Some(ref status_desc) = provider.status_description {
                        ui.add_space(2.0);
                        ui.horizontal(|ui| {
                            let status_col = status_color(provider.status_level);
                            ui.label(
                                RichText::new(format!("Status: {}", status_desc))
                                    .size(FontSize::XS)
                                    .color(status_col),
                            );
                        });
                    }
                }
            });
        });

        // ═══════════════════════════════════════════════════════════════════
        // DIVIDER - only if we have metrics
        // ═══════════════════════════════════════════════════════════════════
        let has_metrics = provider.session_percent.is_some() || provider.weekly_percent.is_some();
        let has_credits = provider.credits_remaining.is_some();
        let has_cost = provider.cost_used.is_some();
        let has_usage_breakdown = !provider.usage_breakdown.is_empty();

        if has_metrics || provider.error.is_some() || has_credits || has_cost || has_usage_breakdown
        {
            ui.add_space(4.0);
            draw_horizontal_separator(ui, 0.0);
        }

        // ═══════════════════════════════════════════════════════════════════
        // METRICS SECTION - macOS style with 12px spacing between metrics
        // ═══════════════════════════════════════════════════════════════════
        if has_metrics {
            ui.add_space(10.0);

            match provider.name.as_str() {
                "claude" => {
                    if let Some(session_pct) = provider.session_percent {
                        let display_percent = usage_display_percent(session_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            &format!(
                                "{}-Sitzung",
                                provider
                                    .source_label
                                    .as_deref()
                                    .unwrap_or("Primary")
                                    .to_uppercase()
                            ),
                            session_pct,
                            show_as_used,
                            provider.session_reset.as_deref(),
                            metric_severity_color(display_percent),
                            content_width,
                            provider.session_pace_percent,
                            provider.session_pace_lasts_to_reset,
                        );
                    }
                    if let Some(weekly_pct) = provider.weekly_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(weekly_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Wöchentlich",
                            weekly_pct,
                            show_as_used,
                            provider.weekly_reset.as_deref(),
                            pace_severity_color(display_percent, provider.pace_percent),
                            content_width,
                            provider.pace_percent,
                            provider.pace_lasts_to_reset,
                        );
                    }
                }
                "codex" => {
                    if let Some(session_pct) = provider.session_percent {
                        let display_percent = usage_display_percent(session_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "5h Window",
                            session_pct,
                            show_as_used,
                            provider.session_reset.as_deref(),
                            metric_severity_color(display_percent),
                            content_width,
                            provider.session_pace_percent,
                            provider.session_pace_lasts_to_reset,
                        );
                    }

                    if let Some(weekly_pct) = provider.weekly_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(weekly_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Weekly",
                            weekly_pct,
                            show_as_used,
                            provider.weekly_reset.as_deref(),
                            metric_severity_color(display_percent),
                            content_width,
                            provider.weekly_pace_percent,
                            provider.weekly_pace_lasts_to_reset,
                        );
                    }

                    ui.add_space(12.0);
                    draw_codex_window_ratio_row(ui, provider);
                }
                "cursor" => {
                    if let Some(auto_pct) = provider.weekly_percent {
                        let display_percent = usage_display_percent(auto_pct, show_as_used);
                        draw_metric_row_with_reset_color(
                            ui,
                            "Auto + Composer",
                            auto_pct,
                            show_as_used,
                            provider.weekly_reset.as_deref(),
                            cursor_metric_color(
                                display_percent,
                                provider.weekly_pace_percent,
                                provider.weekly_pace_lasts_to_reset,
                            ),
                            content_width,
                            provider.weekly_pace_percent,
                            provider.weekly_pace_lasts_to_reset,
                            cursor_reset_time_color(auto_pct, provider.weekly_pace_percent),
                        );
                    }

                    if let Some(api_pct) = provider.model_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(api_pct, show_as_used);
                        draw_metric_row_with_reset_color(
                            ui,
                            "API",
                            api_pct,
                            show_as_used,
                            provider.model_reset.as_deref(),
                            tinted(
                                cursor_metric_color(
                                    display_percent,
                                    provider.model_pace_percent,
                                    provider.model_pace_lasts_to_reset,
                                ),
                                210,
                            ),
                            content_width,
                            provider.model_pace_percent,
                            provider.model_pace_lasts_to_reset,
                            cursor_reset_time_color(api_pct, provider.model_pace_percent),
                        );
                    }

                    if let Some(total_pct) = provider.session_percent {
                        ui.add_space(12.0);
                        draw_metric_summary_row_with_reset_color(
                            ui,
                            "Total",
                            total_pct,
                            show_as_used,
                            provider.session_reset.as_deref(),
                            provider
                                .session_percent
                                .map(|percent| {
                                    metric_severity_color(usage_display_percent(
                                        percent,
                                        show_as_used,
                                    ))
                                })
                                .unwrap_or(Theme::TEXT_PRIMARY),
                            cursor_reset_time_color(total_pct, provider.session_pace_percent),
                        );
                    }
                }
                "gemini" => {
                    if let Some(pro_pct) = provider.session_percent {
                        let display_percent = usage_display_percent(pro_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Gemini Pro",
                            pro_pct,
                            show_as_used,
                            provider.session_reset.as_deref(),
                            metric_severity_color(display_percent),
                            content_width,
                            None,
                            false,
                        );
                    }

                    if let Some(flash_pct) = provider.weekly_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(flash_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Gemini Flash",
                            flash_pct,
                            show_as_used,
                            provider.weekly_reset.as_deref(),
                            tinted(metric_severity_color(display_percent), 210),
                            content_width,
                            None,
                            false,
                        );
                    }

                    if let Some(other_pct) = provider.model_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(other_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            provider.model_name.as_deref().unwrap_or("Other Models"),
                            other_pct,
                            show_as_used,
                            None,
                            tinted(metric_severity_color(display_percent), 190),
                            content_width,
                            None,
                            false,
                        );
                    }
                }
                "antigravity" => {
                    if let Some(claude_pct) = provider.session_percent {
                        let display_percent = usage_display_percent(claude_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Claude",
                            claude_pct,
                            show_as_used,
                            provider.session_reset.as_deref(),
                            metric_severity_color(display_percent),
                            content_width,
                            None,
                            false,
                        );
                    }

                    if let Some(gemini_pro_pct) = provider.weekly_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(gemini_pro_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Gemini Pro",
                            gemini_pro_pct,
                            show_as_used,
                            provider.weekly_reset.as_deref(),
                            tinted(metric_severity_color(display_percent), 210),
                            content_width,
                            None,
                            false,
                        );
                    }

                    if let Some(gemini_flash_pct) = provider.model_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(gemini_flash_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Gemini Flash",
                            gemini_flash_pct,
                            show_as_used,
                            None,
                            tinted(metric_severity_color(display_percent), 190),
                            content_width,
                            None,
                            false,
                        );
                    }
                }
                _ => {
                    if let Some(session_pct) = provider.session_percent {
                        let display_percent = usage_display_percent(session_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            "Session",
                            session_pct,
                            show_as_used,
                            provider.session_reset.as_deref(),
                            metric_severity_color(display_percent),
                            content_width,
                            None,
                            false,
                        );
                    }

                    if let Some(weekly_pct) = provider.weekly_percent {
                        ui.add_space(12.0);
                        let display_percent = usage_display_percent(weekly_pct, show_as_used);

                        draw_metric_row(
                            ui,
                            "Weekly",
                            weekly_pct,
                            show_as_used,
                            provider.weekly_reset.as_deref(),
                            pace_severity_color(display_percent, provider.pace_percent),
                            content_width,
                            provider.pace_percent,
                            provider.pace_lasts_to_reset,
                        );
                    }

                    if let Some(model_pct) = provider.model_percent {
                        ui.add_space(12.0);

                        let model_label = provider.model_name.as_deref().unwrap_or("Model");
                        let display_percent = usage_display_percent(model_pct, show_as_used);
                        draw_metric_row(
                            ui,
                            model_label,
                            model_pct,
                            show_as_used,
                            None,
                            metric_severity_color(display_percent),
                            content_width,
                            None,
                            false,
                        );
                    }
                }
            }

            ui.add_space(2.0);
        } else if provider.error.is_some() {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    RichText::new("Unable to fetch usage")
                        .size(FontSize::SM)
                        .color(Theme::TEXT_SECONDARY),
                );
            });
            ui.add_space(2.0);
        }

        if !provider.usage_trends.is_empty() {
            if has_metrics {
                draw_horizontal_separator(ui, 0.0);
            }
            ui.add_space(10.0);
            draw_usage_trend_log(ui, provider);
        }

        if let Some(tokens) = &provider.codex_tokens {
            draw_horizontal_separator(ui, 0.0);
            ui.add_space(10.0);
            draw_codex_token_log(ui, tokens);
        }

        // ═══════════════════════════════════════════════════════════════════
        // CREDITS SECTION - macOS CreditsBarContent style
        // ═══════════════════════════════════════════════════════════════════
        if show_credits_extra {
            if let Some(credits) = provider.credits_remaining {
                if has_metrics {
                    draw_horizontal_separator(ui, 0.0);
                }
                ui.add_space(12.0);

                let bar_width = ui.available_width();

                // Title: "Credits" - .font(.body).fontWeight(.medium)
                ui.label(
                    RichText::new("Credits")
                        .size(FontSize::BASE)
                        .color(Theme::TEXT_PRIMARY)
                        .strong(),
                );

                // Progress bar
                if let Some(credits_pct) = provider.credits_percent {
                    ui.add_space(6.0);
                    let bar_height = 8.0;
                    let (rect, _) = ui.allocate_exact_size(
                        Vec2::new(bar_width, bar_height),
                        egui::Sense::hover(),
                    );

                    ui.painter()
                        .rect_filled(rect, Rounding::same(4.0), Theme::progress_track());

                    let fill_w = rect.width() * (credits_pct as f32 / 100.0).clamp(0.0, 1.0);
                    if fill_w > 0.0 {
                        let fill_rect =
                            Rect::from_min_size(rect.min, Vec2::new(fill_w, bar_height));
                        ui.painter()
                            .rect_filled(fill_rect, Rounding::same(4.0), brand_color);
                    }
                }

                // Info row: X left (left) | 1K tokens (right) - .font(.caption)
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{:.2} left", credits))
                            .size(FontSize::XS)
                            .color(Theme::TEXT_PRIMARY),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("1K tokens")
                                .size(FontSize::XS)
                                .color(Theme::TEXT_SECONDARY),
                        );
                    });
                });

                // Buy Credits link
                ui.add_space(6.0);
                if draw_menu_item(ui, "⊕", "Buy Credits...") {
                    if let Some(ref url) = provider.dashboard_url {
                        let _ = open::that(url);
                    }
                }

                // Credits history chart
                if !provider.credits_history.is_empty() {
                    ui.add_space(8.0);
                    let chart_points: Vec<ChartPoint> = provider
                        .credits_history
                        .iter()
                        .map(|(date, value)| ChartPoint::new(date.clone(), *value))
                        .collect();
                    let mut chart = CreditsHistoryChart::new(chart_points);
                    chart.show(ui);
                }
            }
        }

        // ═══════════════════════════════════════════════════════════════════
        // USAGE BREAKDOWN SECTION - stacked service credits chart
        // ═══════════════════════════════════════════════════════════════════
        if has_usage_breakdown {
            if has_metrics || has_credits {
                draw_horizontal_separator(ui, 0.0);
            }
            ui.add_space(12.0);

            ui.label(
                RichText::new("Usage breakdown")
                    .size(FontSize::BASE)
                    .color(Theme::TEXT_PRIMARY)
                    .strong(),
            );
            ui.add_space(6.0);

            let mut chart = UsageBreakdownChart::new(provider.usage_breakdown.clone());
            chart.show(ui);
        }

        // ═══════════════════════════════════════════════════════════════════
        // COST SECTION - macOS TokenUsageSection style
        // ═══════════════════════════════════════════════════════════════════
        if show_credits_extra && (provider.cost_used.is_some() || !provider.cost_history.is_empty())
        {
            if has_metrics || has_credits || has_usage_breakdown {
                draw_horizontal_separator(ui, 0.0);
            }
            ui.add_space(12.0);

            // Title: "Cost" - .font(.body).fontWeight(.medium)
            ui.label(
                RichText::new("Cost")
                    .size(FontSize::BASE)
                    .color(Theme::TEXT_PRIMARY)
                    .strong(),
            );

            ui.add_space(6.0);

            // Cost details - Today and Last 30 days - .font(.caption)
            if !provider.cost_history.is_empty() {
                let total_30d: f64 = provider.cost_history.iter().map(|(_, cost)| cost).sum();
                let today_cost: f64 = provider.cost_history.last().map(|(_, c)| *c).unwrap_or(0.0);

                ui.label(
                    RichText::new(format!("Today: ${:.2}", today_cost))
                        .size(FontSize::XS)
                        .color(Theme::TEXT_PRIMARY),
                );
                ui.label(
                    RichText::new(format!("Last 30 days: ${:.2}", total_30d))
                        .size(FontSize::XS)
                        .color(Theme::TEXT_PRIMARY),
                );
            } else if let Some(cost_used) = &provider.cost_used {
                ui.label(
                    RichText::new(cost_used)
                        .size(FontSize::XS)
                        .color(Theme::TEXT_PRIMARY),
                );
            }

            // Cost history chart
            if !provider.cost_history.is_empty() {
                ui.add_space(8.0);
                let chart_points: Vec<ChartPoint> = provider
                    .cost_history
                    .iter()
                    .map(|(date, cost)| ChartPoint::new(date.clone(), *cost))
                    .collect();
                let mut chart = CostHistoryChart::new(chart_points, brand_color);
                chart.show(ui);
            }
        }

        // ═══════════════════════════════════════════════════════════════════
        // ACTION LINKS SECTION - macOS style vertical list
        // ═══════════════════════════════════════════════════════════════════
        let has_dashboard = provider.dashboard_url.is_some();
        let has_status_issue = provider.status_level != StatusLevel::Operational;
        let has_error = provider.error.is_some();

        if has_dashboard || has_status_issue || has_error {
            if has_metrics || has_credits || has_cost || has_usage_breakdown {
                draw_horizontal_separator(ui, 0.0);
            }
            ui.add_space(6.0);

            // Vertical action links like macOS
            // Refresh button - first action
            if draw_menu_item(ui, "↻", "Refresh") {
                refresh_requested = true;
            }

            // Switch Account link - only show for providers that support token accounts
            if TokenAccountSupport::is_supported(
                ProviderId::from_cli_name(&provider.name).unwrap_or(ProviderId::Claude),
            ) {
                if draw_menu_item(ui, "->", "Switch Account...") {
                    account_switch_requested = Some(provider.name.clone());
                }
            }

            // Usage Dashboard link
            if let Some(ref url) = provider.dashboard_url {
                let dashboard_url = url.clone();
                if draw_menu_item(ui, "📊", "Usage Dashboard") {
                    let _ = open::that(&dashboard_url);
                }
            }

            // Status Page link
            if let Some(status_url) = get_status_page_url(&provider.name) {
                if draw_menu_item(ui, "⚡", "Status Page") {
                    let _ = open::that(status_url);
                }
            }

            // Copy Error link
            if let Some(ref error) = provider.error {
                let error_text = error.clone();
                if draw_menu_item(ui, "📋", "Copy Error") {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(&error_text);
                    }
                }
            }

            ui.add_space(4.0);
        }

        (refresh_requested, account_switch_requested)
    })
    .inner
}

/// Draw a horizontal separator with left padding
fn draw_horizontal_separator(ui: &mut egui::Ui, left_padding: f32) {
    ui.horizontal(|ui| {
        ui.add_space(left_padding);
        let sep_rect = ui.available_rect_before_wrap();
        let sep_width = sep_rect.width() - left_padding;
        ui.painter().hline(
            sep_rect.left()..=(sep_rect.left() + sep_width),
            sep_rect.top(),
            Stroke::new(1.0, Theme::SEPARATOR),
        );
    });
}

fn draw_metric_summary_row_with_reset_color(
    ui: &mut egui::Ui,
    title: &str,
    percent: f64,
    show_as_used: bool,
    reset_text: Option<&str>,
    value_color: Color32,
    reset_color: Option<Color32>,
) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .size(FontSize::BASE)
                .color(Theme::TEXT_PRIMARY)
                .strong(),
        );
        ui.add_space(8.0);
        ui.label(
            RichText::new(usage_display_label(
                usage_display_percent(percent, show_as_used),
                show_as_used,
            ))
            .size(FontSize::SM)
            .color(value_color),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(reset) = reset_text {
                ui.label(
                    RichText::new(format!("Resets in {}", reset))
                        .size(FontSize::XS)
                        .color(reset_color.unwrap_or(Theme::TEXT_SECONDARY)),
                );
            }
        });
    });
}

fn draw_codex_window_ratio_row(ui: &mut egui::Ui, provider: &ProviderData) {
    let (label, detail, value_color) = codex_window_ratio_label(provider.codex_window_ratio);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("5h vs Weekly")
                .size(FontSize::BASE)
                .color(Theme::TEXT_PRIMARY)
                .strong(),
        );
        ui.add_space(8.0);
        ui.label(RichText::new(label).size(FontSize::SM).color(value_color));
        if let Some(detail) = detail {
            ui.label(
                RichText::new(format!("· {}", detail))
                    .size(FontSize::XS)
                    .color(Theme::TEXT_SECONDARY),
            );
        }
    });

    if let (Some(ratio), Some(weekly_used)) = (provider.codex_window_ratio, provider.weekly_percent)
    {
        ui.add_space(6.0);
        draw_codex_window_dots(ui, ratio, weekly_used);
    } else {
        ui.add_space(3.0);
        ui.label(
            RichText::new("Refresh after real Codex usage to estimate the 5h-to-weekly ratio.")
                .size(FontSize::XS)
                .color(Theme::TEXT_SECONDARY),
        );
    }
}

fn draw_codex_window_dots(ui: &mut egui::Ui, ratio: CodexWindowRatio, weekly_used: f64) {
    let total = ratio.estimated_windows.round().clamp(1.0, 20.0) as usize;
    let used_windows = (weekly_used / ratio.weekly_per_full_session).clamp(0.0, total as f64);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(3.0, 2.0);
        for idx in 0..total {
            let threshold = ((idx + 1) as f64 / total as f64) * 100.0;
            let color = if used_windows >= (idx + 1) as f64 {
                metric_severity_color(threshold)
            } else if used_windows > idx as f64 {
                metric_severity_color(threshold).linear_multiply(0.75)
            } else {
                Theme::progress_track()
            };
            ui.label(RichText::new("●").size(FontSize::SM).color(color));
        }
    });
}

fn draw_usage_trend_log(ui: &mut egui::Ui, provider: &ProviderData) {
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new("Usage trend")
                    .size(FontSize::BASE)
                    .color(Theme::TEXT_PRIMARY)
                    .strong(),
            );
            ui.add_space(5.0);

            for trend in &provider.usage_trends {
                let color = trend_comparison_color(trend.comparison);
                let comparison = trend_comparison_label(trend.comparison);
                let label = trend_metric_label(&provider.name, trend.label);
                let delta = format_signed_percent(trend.delta_percent);
                let per_hour = format_signed_percent(trend.per_hour);
                let avg_24h = trend
                    .average_per_day_24h
                    .map(format_signed_percent)
                    .unwrap_or_else(|| "n/a".to_string());
                let avg_7d = trend
                    .average_per_day_7d
                    .map(format_signed_percent)
                    .unwrap_or_else(|| "n/a".to_string());

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("{}:", label))
                            .size(FontSize::XS)
                            .color(Theme::TEXT_SECONDARY),
                    );
                    ui.label(
                        RichText::new(delta)
                            .size(FontSize::XS)
                            .color(color)
                            .strong(),
                    );
                    ui.label(
                        RichText::new(format!(
                            "· {}/h · 24h {}/d · 7d {}/d · {} · n={}",
                            per_hour, avg_24h, avg_7d, comparison, trend.sample_count
                        ))
                        .size(FontSize::XS)
                        .color(Theme::TEXT_SECONDARY),
                    );
                });
            }
        });
    });
}

fn draw_codex_token_log(ui: &mut egui::Ui, tokens: &CodexTokenLogSample) {
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.vertical(|ui| {
            ui.label(
                RichText::new("Codex tokens")
                    .size(FontSize::BASE)
                    .color(Theme::TEXT_PRIMARY)
                    .strong(),
            );
            ui.add_space(5.0);

            let model = tokens.last_model.as_deref().unwrap_or("unknown");
            ui.label(
                RichText::new(format!("Last model: {}", model))
                    .size(FontSize::XS)
                    .color(Theme::TEXT_SECONDARY),
            );
            ui.label(
                RichText::new(format!(
                    "Delta: {} total · {} in · {} cached · {} out",
                    format_token_count(tokens.total_tokens_delta),
                    format_token_count(tokens.input_tokens_delta),
                    format_token_count(tokens.cached_tokens_delta),
                    format_token_count(tokens.output_tokens_delta)
                ))
                .size(FontSize::XS)
                .color(Theme::TEXT_PRIMARY),
            );
            ui.label(
                RichText::new(format!(
                    "30d total: {} tokens",
                    format_token_count(tokens.total_tokens_total)
                ))
                .size(FontSize::XS)
                .color(Theme::TEXT_SECONDARY),
            );
        });
    });
}

fn format_token_count(tokens: i64) -> String {
    if tokens.abs() >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens.abs() >= 1_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

fn trend_metric_label(provider_name: &str, label: &str) -> &'static str {
    match (provider_name, label) {
        ("codex", "Primary") => "5h",
        ("codex", "Secondary") => "Week",
        ("cursor", "Primary") => "Total",
        ("cursor", "Secondary") => "Auto",
        ("cursor", "Model") => "API",
        ("gemini", "Primary") => "Pro",
        ("gemini", "Secondary") => "Flash",
        ("gemini", "Model") => "Other",
        ("antigravity", "Primary") => "Claude",
        ("antigravity", "Secondary") => "Gemini Pro",
        ("antigravity", "Model") => "Gemini Flash",
        (_, "Primary") => "Primary",
        (_, "Secondary") => "Secondary",
        (_, "Model") => "Model",
        _ => "Usage",
    }
}

fn format_signed_percent(value: f64) -> String {
    if value > 0.0 {
        format!("+{:.1}%", value)
    } else {
        format!("{:.1}%", value)
    }
}

fn trend_comparison_label(comparison: UsageTrendComparison) -> &'static str {
    match comparison {
        UsageTrendComparison::Faster => "faster",
        UsageTrendComparison::Slower => "slower",
        UsageTrendComparison::Steady => "steady",
        UsageTrendComparison::ResetOrDrop => "reset/drop",
        UsageTrendComparison::Insufficient => "learning",
    }
}

fn trend_comparison_color(comparison: UsageTrendComparison) -> Color32 {
    match comparison {
        UsageTrendComparison::Faster => Theme::ORANGE,
        UsageTrendComparison::Slower | UsageTrendComparison::ResetOrDrop => Theme::GREEN,
        UsageTrendComparison::Steady => Theme::TEXT_PRIMARY,
        UsageTrendComparison::Insufficient => Theme::TEXT_SECONDARY,
    }
}

fn section_header_inline(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title)
            .size(FontSize::XS)
            .color(Theme::TEXT_MUTED)
            .strong(),
    );
}

/// Draw a text-only menu item (Settings, About, Quit style)
fn draw_text_menu_item(ui: &mut egui::Ui, label: &str) -> bool {
    let available_width = ui.available_width();

    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(available_width, 24.0), egui::Sense::click());

    let is_hovered = response.hovered();

    if is_hovered {
        ui.painter()
            .rect_filled(rect, Rounding::same(Radius::SM), Theme::menu_hover());
    }

    let text_color = if is_hovered {
        Theme::TEXT_PRIMARY
    } else {
        Theme::TEXT_SECONDARY
    };

    // Label
    let label_pos = egui::pos2(rect.min.x + 4.0, rect.center().y);
    ui.painter().text(
        label_pos,
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(FontSize::SM),
        text_color,
    );

    response.clicked()
}

/// Draw a single metric row - macOS style matching SwiftUI MetricRow
/// Structure: Title (.body.medium) → Progress bar (with optional pace marker) → X% used | Pace status | Resets in Xh (.footnote)
///
/// # Arguments
/// * `pace_percent` - Optional pace difference (actual - expected). Positive means ahead of expected, negative means behind.
/// * `pace_lasts_to_reset` - Whether current usage will last until reset (on track or ahead)
fn draw_metric_row(
    ui: &mut egui::Ui,
    title: &str,
    percent: f64,
    show_as_used: bool,
    reset_text: Option<&str>,
    color: Color32,
    _content_width: f32,
    pace_percent: Option<f64>,
    pace_lasts_to_reset: bool,
) {
    draw_metric_row_with_reset_color(
        ui,
        title,
        percent,
        show_as_used,
        reset_text,
        color,
        _content_width,
        pace_percent,
        pace_lasts_to_reset,
        None,
    );
}

fn draw_metric_row_with_reset_color(
    ui: &mut egui::Ui,
    title: &str,
    percent: f64,
    show_as_used: bool,
    reset_text: Option<&str>,
    color: Color32,
    _content_width: f32,
    pace_percent: Option<f64>,
    pace_lasts_to_reset: bool,
    reset_color_override: Option<Color32>,
) {
    draw_metric_row_with_pace_mode(
        ui,
        title,
        percent,
        show_as_used,
        reset_text,
        color,
        _content_width,
        pace_percent,
        pace_lasts_to_reset,
        reset_color_override,
    );
}

fn draw_metric_row_with_pace_mode(
    ui: &mut egui::Ui,
    title: &str,
    percent: f64,
    show_as_used: bool,
    reset_text: Option<&str>,
    color: Color32,
    _content_width: f32,
    pace_percent: Option<f64>,
    pace_lasts_to_reset: bool,
    reset_color_override: Option<Color32>,
) {
    // Title - .font(.body).fontWeight(.medium)
    ui.label(
        RichText::new(title)
            .size(FontSize::BASE)
            .color(Theme::TEXT_PRIMARY)
            .strong(),
    );

    ui.add_space(6.0);

    let display_percent = usage_display_percent(percent, show_as_used);
    let display_pace_percent = pace_percent.map(|pace| if show_as_used { pace } else { -pace });

    // Progress bar row - 8px height like macOS
    let bar_width = ui.available_width();
    let bar_height = 8.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(bar_width, bar_height), egui::Sense::hover());

    // Track
    ui.painter()
        .rect_filled(rect, Rounding::same(4.0), Theme::progress_track());

    // Fill
    let fill_w = rect.width() * (display_percent as f32 / 100.0).clamp(0.0, 1.0);
    if fill_w > 0.0 {
        let fill_rect = Rect::from_min_size(rect.min, Vec2::new(fill_w, bar_height));
        ui.painter()
            .rect_filled(fill_rect, Rounding::same(4.0), color);
    }

    // Pace marker - thin vertical line showing expected usage position
    if let Some(pace_diff) = display_pace_percent {
        // pace_percent is the difference (actual - expected), so expected = actual - pace_diff
        let expected_position = (display_percent - pace_diff).clamp(0.0, 100.0);
        let marker_x = rect.min.x + rect.width() * (expected_position as f32 / 100.0);

        // Draw 2px wide vertical line in a contrasting color (white with transparency)
        let marker_color = Color32::from_rgba_unmultiplied(255, 255, 255, 180);
        let marker_width = 2.0;
        let marker_rect = Rect::from_min_size(
            egui::pos2(marker_x - marker_width / 2.0, rect.min.y),
            Vec2::new(marker_width, bar_height),
        );
        ui.painter()
            .rect_filled(marker_rect, Rounding::same(1.0), marker_color);
    }

    ui.add_space(6.0);

    // Info row: X% used (left) | Pace status | Resets in Xh (right) - .font(.footnote)
    ui.horizontal(|ui| {
        let usage_text_color = color;
        ui.label(
            RichText::new(usage_display_label(display_percent, show_as_used))
                .size(FontSize::XS)
                .color(usage_text_color),
        );

        // Pace status indicator
        if let Some((pace_text, pace_color)) =
            pace_status_with_mode(display_percent, pace_percent, pace_lasts_to_reset)
        {
            ui.add_space(8.0);
            ui.label(
                RichText::new(pace_text)
                    .size(FontSize::XS)
                    .color(pace_color),
            );
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(reset) = reset_text {
                let reset_color = reset_color_override.unwrap_or_else(|| {
                    pace_status_with_mode(display_percent, pace_percent, pace_lasts_to_reset)
                        .map(|(_, color)| color)
                        .unwrap_or(Theme::TEXT_SECONDARY)
                });
                ui.label(
                    RichText::new(format!("Resets in {}", reset))
                        .size(FontSize::XS)
                        .color(reset_color),
                );
            }
        });
    });
}

/// Draw a menu item button - macOS style compact
fn draw_menu_item(ui: &mut egui::Ui, icon: &str, label: &str) -> bool {
    let available_width = ui.available_width();

    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(available_width, 32.0), // Slightly larger height
        egui::Sense::click(),
    );

    let is_hovered = response.hovered();

    if is_hovered {
        ui.painter()
            .rect_filled(rect, Rounding::same(Radius::SM), Theme::menu_hover());
    }

    let text_color = if is_hovered {
        Theme::TEXT_PRIMARY
    } else {
        Theme::TEXT_SECONDARY
    };

    // Icon
    let icon_pos = egui::pos2(rect.min.x + Spacing::SM, rect.center().y);
    ui.painter().text(
        icon_pos,
        egui::Align2::LEFT_CENTER,
        icon,
        egui::FontId::proportional(FontSize::MD),
        text_color,
    );

    // Label
    let label_pos = egui::pos2(rect.min.x + Spacing::SM + 22.0, rect.center().y);
    ui.painter().text(
        label_pos,
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(FontSize::SM),
        text_color,
    );

    response.clicked()
}

fn load_window_icon() -> Option<egui::IconData> {
    let png_bytes = include_bytes!("../../icons/icon.png");
    let image = image::load_from_memory_with_format(png_bytes, image::ImageFormat::Png)
        .ok()?
        .into_rgba8();
    let (width, height) = image.dimensions();

    Some(egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    })
}

/// Run the application
pub fn run() -> anyhow::Result<()> {
    let settings = Settings::load();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([760.0, 900.0])
        .with_min_inner_size([680.0, 820.0])
        .with_clamp_size_to_monitor_size(true)
        .with_resizable(true)
        .with_decorations(true)
        .with_transparent(false);

    if let Some(icon) = load_window_icon() {
        viewport = viewport.with_icon(icon);
    }

    viewport = viewport.with_title("CodexBar");
    if settings.always_on_top {
        viewport = viewport.with_always_on_top();
    }
    let options = eframe::NativeOptions {
        viewport,
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "CodexBar",
        options,
        Box::new(|cc| Ok(Box::new(CodexBarApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {}", e))?;

    Ok(())
}
