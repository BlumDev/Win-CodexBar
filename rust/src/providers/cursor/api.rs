//! Cursor API client for fetching usage information
//!
//! Uses browser cookies to authenticate with cursor.com API

use crate::browser::cookies::get_cookie_header;
use crate::core::{CostSnapshot, ProviderError, RateWindow};
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use std::path::PathBuf;

const BASE_URL: &str = "https://cursor.com";
const DESKTOP_API_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";
const COOKIE_DOMAINS: [&str; 2] = ["cursor.com", "cursor.sh"];

type CursorUsageResult = (
    RateWindow,
    Option<RateWindow>,
    Option<RateWindow>,
    Option<CostSnapshot>,
    Option<String>,
    Option<String>,
);

/// Cursor API client
pub struct CursorApi {
    client: reqwest::Client,
}

impl CursorApi {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    /// Fetch usage information from Cursor API
    /// Returns (primary, secondary, model_specific, cost, email, plan_type)
    pub async fn fetch_usage(&self) -> Result<(CursorUsageResult, &'static str), ProviderError> {
        match self.fetch_usage_from_desktop().await {
            Ok(result) => return Ok((result, "desktop")),
            Err(error) => {
                tracing::debug!("Cursor desktop auth unavailable: {}", error);
            }
        }

        let cookie_header = self.get_cookie_header()?;
        self.fetch_usage_with_cookie_header(&cookie_header)
            .await
            .map(|result| (result, "web"))
    }

    async fn fetch_usage_from_desktop(&self) -> Result<CursorUsageResult, ProviderError> {
        let auth = load_cursor_desktop_auth()?.ok_or(ProviderError::NoCookies)?;
        let response = self
            .client
            .post(DESKTOP_API_URL)
            .bearer_auth(&auth.access_token)
            .header("Accept", "application/json")
            .json(&serde_json::json!({}))
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        if response.status() == 401 || response.status() == 403 {
            return Err(ProviderError::AuthRequired);
        }
        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Cursor desktop API returned {}",
                response.status()
            )));
        }

        let mut summary: UsageSummary = response
            .json()
            .await
            .map_err(|error| ProviderError::Parse(error.to_string()))?;
        summary.membership_type = auth.membership_type;
        self.build_result(summary, None)
    }

    /// Fetch usage information using an explicit cookie header
    pub async fn fetch_usage_with_cookie_header(
        &self,
        cookie_header: &str,
    ) -> Result<CursorUsageResult, ProviderError> {
        if cookie_header.trim().is_empty() {
            return Err(ProviderError::NoCookies);
        }

        // Fetch usage summary and user info in parallel
        let (usage_result, user_result) = tokio::join!(
            self.fetch_usage_summary(cookie_header),
            self.fetch_user_info(cookie_header)
        );

        let usage_summary = usage_result?;
        let user_info = user_result.ok();

        self.build_result(usage_summary, user_info)
    }

    fn get_cookie_header(&self) -> Result<String, ProviderError> {
        for domain in COOKIE_DOMAINS {
            match get_cookie_header(domain) {
                Ok(header) if !header.is_empty() => {
                    tracing::debug!("Found Cursor cookies for {}", domain);
                    return Ok(header);
                }
                Ok(_) => {
                    tracing::debug!("No cookies for {}", domain);
                }
                Err(e) => {
                    tracing::debug!("Cookie error for {}: {}", domain, e);
                }
            }
        }

        Err(ProviderError::NoCookies)
    }

    async fn fetch_usage_summary(
        &self,
        cookie_header: &str,
    ) -> Result<UsageSummary, ProviderError> {
        let url = format!("{}/api/usage-summary", BASE_URL);

        let response = self
            .client
            .get(&url)
            .header("Cookie", cookie_header)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        if response.status() == 401 || response.status() == 403 {
            return Err(ProviderError::AuthRequired);
        }

        if !response.status().is_success() {
            return Err(ProviderError::Other(format!(
                "Cursor API returned {}",
                response.status()
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))
    }

    async fn fetch_user_info(&self, cookie_header: &str) -> Result<UserInfo, ProviderError> {
        let url = format!("{}/api/auth/me", BASE_URL);

        let response = self
            .client
            .get(&url)
            .header("Cookie", cookie_header)
            .header("Accept", "application/json")
            .timeout(std::time::Duration::from_secs(15))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(ProviderError::Other(
                "Failed to fetch user info".to_string(),
            ));
        }

        response
            .json()
            .await
            .map_err(|e| ProviderError::Parse(e.to_string()))
    }

    fn build_result(
        &self,
        summary: UsageSummary,
        user_info: Option<UserInfo>,
    ) -> Result<CursorUsageResult, ProviderError> {
        let billing_end = summary
            .billing_cycle_end
            .as_ref()
            .and_then(|s| parse_cursor_date(s));
        let billing_start = summary
            .billing_cycle_start
            .as_ref()
            .and_then(|s| parse_cursor_date(s));
        let billing_window_minutes = billing_window_minutes(billing_start, billing_end);

        let (percent_used, secondary, model_specific, cost_snapshot) =
            if let Some(plan) = summary.plan_usage.as_ref() {
                let total_spend_cents = plan.total_spend.unwrap_or(0) as f64;
                let included_spend_cents = plan.included_spend.unwrap_or(0) as f64;
                let limit_cents = plan.limit.unwrap_or(0) as f64;
                let percent = if limit_cents > 0.0 {
                    (included_spend_cents / limit_cents) * 100.0
                } else {
                    plan.total_percent_used
                        .map(normalize_cursor_percent)
                        .unwrap_or(0.0)
                };
                let secondary = plan.auto_percent_used.map(|value| {
                    RateWindow::with_details(
                        normalize_cursor_percent(value),
                        billing_window_minutes,
                        billing_end,
                        None,
                    )
                });
                let model_specific = plan.api_percent_used.map(|value| {
                    RateWindow::with_details(
                        normalize_cursor_percent(value),
                        billing_window_minutes,
                        billing_end,
                        None,
                    )
                });
                let mut cost = CostSnapshot::new(total_spend_cents / 100.0, "USD", "Monthly");
                if limit_cents > 0.0 {
                    cost = cost.with_limit(limit_cents / 100.0);
                }
                if let Some(reset) = billing_end {
                    cost = cost.with_resets_at(reset);
                }
                (percent, secondary, model_specific, Some(cost))
            } else if let Some(individual) = &summary.individual_usage {
                if let Some(plan) = &individual.plan {
                    let used_cents = plan.used.unwrap_or(0) as f64;
                    let limit_cents = plan
                        .breakdown
                        .as_ref()
                        .and_then(|b| b.total)
                        .or(plan.limit)
                        .unwrap_or(0) as f64;

                    let percent = if let Some(total_percent) = plan.total_percent_used {
                        normalize_cursor_percent(total_percent)
                    } else if summary.is_unlimited == Some(true) {
                        0.0
                    } else if limit_cents > 0.0 {
                        (used_cents / limit_cents) * 100.0
                    } else {
                        0.0
                    };

                    let secondary = plan.auto_percent_used.map(|v| {
                        RateWindow::with_details(
                            normalize_cursor_percent(v),
                            billing_window_minutes,
                            billing_end,
                            None,
                        )
                    });

                    let model_specific = plan.api_percent_used.map(|v| {
                        RateWindow::with_details(
                            normalize_cursor_percent(v),
                            billing_window_minutes,
                            billing_end,
                            None,
                        )
                    });

                    let mut cost = CostSnapshot::new(used_cents / 100.0, "USD", "Monthly");
                    if limit_cents > 0.0 {
                        cost = cost.with_limit(limit_cents / 100.0);
                    }
                    if let Some(reset) = billing_end {
                        cost = cost.with_resets_at(reset);
                    }

                    (percent, secondary, model_specific, Some(cost))
                } else {
                    (0.0, None, None, None)
                }
            } else {
                (0.0, None, None, None)
            };

        let primary =
            RateWindow::with_details(percent_used, billing_window_minutes, billing_end, None);

        let plan_type = summary
            .membership_type
            .as_ref()
            .map(|t| match t.to_lowercase().as_str() {
                "enterprise" => "Cursor Enterprise".to_string(),
                "pro" => "Cursor Pro".to_string(),
                "hobby" => "Cursor Hobby".to_string(),
                "team" => "Cursor Team".to_string(),
                other => format!("Cursor {}", capitalize(other)),
            });

        let email = user_info.as_ref().and_then(|u| u.email.clone());

        Ok((
            primary,
            secondary,
            model_specific,
            cost_snapshot,
            email,
            plan_type,
        ))
    }
}

impl Default for CursorApi {
    fn default() -> Self {
        Self::new()
    }
}

// --- API Response Types ---

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UsageSummary {
    billing_cycle_start: Option<String>,
    billing_cycle_end: Option<String>,
    membership_type: Option<String>,
    limit_type: Option<String>,
    is_unlimited: Option<bool>,
    individual_usage: Option<IndividualUsage>,
    plan_usage: Option<PlanUsage>,
    team_usage: Option<TeamUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IndividualUsage {
    plan: Option<PlanUsage>,
    on_demand: Option<OnDemandUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanUsage {
    enabled: Option<bool>,
    used: Option<i64>,
    limit: Option<i64>,
    remaining: Option<i64>,
    breakdown: Option<PlanBreakdown>,
    auto_percent_used: Option<f64>,
    api_percent_used: Option<f64>,
    total_percent_used: Option<f64>,
    total_spend: Option<i64>,
    included_spend: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanBreakdown {
    included: Option<i64>,
    bonus: Option<i64>,
    total: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OnDemandUsage {
    enabled: Option<bool>,
    used: Option<i64>,
    limit: Option<i64>,
    remaining: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamUsage {
    on_demand: Option<OnDemandUsage>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    email: Option<String>,
    email_verified: Option<bool>,
    name: Option<String>,
    sub: Option<String>,
    created_at: Option<String>,
    updated_at: Option<String>,
    picture: Option<String>,
}

// --- Helper functions ---

fn parse_cursor_date(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(milliseconds) = s.parse::<i64>() {
        return Utc.timestamp_millis_opt(milliseconds).single();
    }

    // Try with fractional seconds
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try without fractional seconds
    if let Ok(dt) = chrono::DateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ") {
        return Some(dt.with_timezone(&Utc));
    }

    None
}

#[derive(Debug)]
struct CursorDesktopAuth {
    access_token: String,
    membership_type: Option<String>,
}

fn cursor_state_db_path() -> Option<PathBuf> {
    dirs::config_dir().map(|path| {
        path.join("Cursor")
            .join("User")
            .join("globalStorage")
            .join("state.vscdb")
    })
}

fn load_cursor_desktop_auth() -> Result<Option<CursorDesktopAuth>, ProviderError> {
    let Some(path) = cursor_state_db_path() else {
        return Ok(None);
    };
    if !path.exists() {
        return Ok(None);
    }

    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| {
            ProviderError::Other(format!("Failed to read Cursor auth store: {error}"))
        })?;
    let access_token: Option<String> = connection
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            ProviderError::Other(format!("Failed to read Cursor access token: {error}"))
        })?;
    let Some(access_token) = access_token.filter(|token| !token.trim().is_empty()) else {
        return Ok(None);
    };
    let membership_type = connection
        .query_row(
            "SELECT value FROM ItemTable WHERE key = 'cursorAuth/stripeMembershipType'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            ProviderError::Other(format!("Failed to read Cursor membership type: {error}"))
        })?;

    Ok(Some(CursorDesktopAuth {
        access_token,
        membership_type,
    }))
}

fn billing_window_minutes(
    billing_start: Option<DateTime<Utc>>,
    billing_end: Option<DateTime<Utc>>,
) -> Option<u32> {
    let (Some(start), Some(end)) = (billing_start, billing_end) else {
        return None;
    };
    let minutes = end.signed_duration_since(start).num_minutes();
    if minutes > 0 {
        Some(minutes.min(u32::MAX as i64) as u32)
    } else {
        None
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().chain(chars).collect(),
    }
}

fn normalize_cursor_percent(value: f64) -> f64 {
    if !value.is_finite() {
        return 0.0;
    }

    // Cursor reports these fields in percentage units, even below 1%.
    value.clamp(0.0, 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api() -> CursorApi {
        CursorApi::new()
    }

    fn parse_summary(json: &str) -> UsageSummary {
        serde_json::from_str(json).expect("fixture should parse")
    }

    #[test]
    fn test_cursor_build_result_preserves_small_percentage_lanes() {
        let json = r#"{
            "billingCycleStart": "2026-03-01T00:00:00Z",
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 1500,
                    "limit": 5000,
                    "totalPercentUsed": 6.15,
                    "autoPercentUsed": 0.0,
                    "apiPercentUsed": 0.2667
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (primary, secondary, model_specific, cost, _email, plan_type) =
            api().build_result(summary, None).unwrap();

        assert!((primary.used_percent - 6.15).abs() < 0.01);

        let sec = secondary.expect("secondary should be present");
        assert!((sec.used_percent).abs() < 0.01);
        assert!(sec.resets_at.is_some());

        let ms = model_specific.expect("model_specific should be present");
        assert!((ms.used_percent - 0.2667).abs() < 0.01);
        assert!(ms.resets_at.is_some());

        assert!(cost.is_some());
        assert_eq!(plan_type.as_deref(), Some("Cursor Pro"));
    }

    #[test]
    fn test_cursor_build_result_accepts_desktop_usage_payload() {
        let json = r#"{
            "billingCycleStart": "1781648722000",
            "billingCycleEnd": "1784240722000",
            "planUsage": {
                "totalSpend": 421,
                "includedSpend": 421,
                "remaining": 1579,
                "limit": 2000,
                "autoPercentUsed": 0.0,
                "apiPercentUsed": 9.355555555555556,
                "totalPercentUsed": 2.158974358974359
            },
            "enabled": true
        }"#;

        let summary = parse_summary(json);
        let (primary, secondary, model_specific, cost, _, _) =
            api().build_result(summary, None).unwrap();

        assert!((primary.used_percent - 21.05).abs() < 0.01);
        assert_eq!(primary.window_minutes, Some(43_200));

        let auto = secondary.expect("desktop payload includes the Auto lane");
        assert!(auto.used_percent.abs() < f64::EPSILON);

        let api_lane = model_specific.expect("desktop payload includes the API lane");
        assert!((api_lane.used_percent - 9.355555555555556).abs() < 0.01);

        let cost = cost.expect("desktop payload includes plan spend");
        assert!((cost.used - 4.21).abs() < 0.01);
        assert_eq!(cost.limit, Some(20.0));
    }

    #[test]
    fn test_cursor_build_result_cents_only() {
        let json = r#"{
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 2500,
                    "limit": 5000
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (primary, secondary, model_specific, cost, _, _) =
            api().build_result(summary, None).unwrap();

        assert!((primary.used_percent - 50.0).abs() < 0.01);
        assert!(secondary.is_none(), "no autoPercentUsed in payload");
        assert!(model_specific.is_none(), "no apiPercentUsed in payload");
        assert!(cost.is_some());
    }

    #[test]
    fn test_cursor_build_result_missing_plan() {
        let json = r#"{
            "membershipType": "hobby",
            "individualUsage": {}
        }"#;

        let summary = parse_summary(json);
        let (primary, secondary, model_specific, cost, _, _) =
            api().build_result(summary, None).unwrap();

        assert!((primary.used_percent).abs() < 0.01);
        assert!(secondary.is_none());
        assert!(model_specific.is_none());
        assert!(cost.is_none());
    }

    #[test]
    fn test_cursor_on_demand_as_cost() {
        let json = r#"{
            "billingCycleEnd": "2026-04-01T00:00:00Z",
            "membershipType": "pro",
            "individualUsage": {
                "plan": {
                    "used": 800,
                    "limit": 5000,
                    "totalPercentUsed": 0.16
                },
                "onDemand": {
                    "enabled": true,
                    "used": 350,
                    "limit": 1000
                }
            }
        }"#;

        let summary = parse_summary(json);
        let (primary, _, _, cost, _, _) = api().build_result(summary, None).unwrap();

        assert!((primary.used_percent - 0.16).abs() < 0.01);
        let cost = cost.expect("cost should exist from plan usage");
        assert!((cost.used - 8.0).abs() < 0.01);
        assert_eq!(cost.limit, Some(50.0));
    }
}
