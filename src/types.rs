use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Ios,
    Android,
    Api,
}

impl Source {
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Ios => "ios",
            Source::Android => "android",
            Source::Api => "api",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IngestEvent {
    pub timestamp: i64,
    pub source: Source,
    pub environment: String,
    pub release: String,
    pub app_version: Option<String>,
    pub build_number: Option<String>,
    pub route_or_procedure: Option<String>,
    pub screen: Option<String>,
    pub error_type: String,
    pub message: String,
    pub stack: Option<String>,
    pub http_status: Option<u16>,
    pub request_id: Option<String>,
    pub user_id_hash: Option<String>,
    pub device_id_hash: Option<String>,
    pub fingerprint: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Internal event with computed fingerprint and receive timestamp.
#[derive(Debug, Clone)]
pub struct ProcessedEvent {
    pub event: IngestEvent,
    pub fingerprint: String,
    pub received_at: i64,
    pub project_id: String,
}

/// Aggregated error summary returned by query endpoints.
#[derive(Debug, Serialize)]
pub struct ErrorAggregate {
    pub project_id: String,
    pub fingerprint: String,
    pub release: String,
    pub environment: String,
    pub total_count: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    pub error_type: String,
    pub message: String,
    pub source: String,
    pub route_or_procedure: Option<String>,
    pub screen: Option<String>,
    pub status: String,
}

/// Sample occurrence for error detail view.
#[derive(Debug, Clone, Serialize)]
pub struct SampleOccurrence {
    pub id: i64,
    pub fingerprint: String,
    pub project_id: String,
    pub captured_at: i64,
    pub source: String,
    pub environment: String,
    pub release: String,
    pub error_type: String,
    pub message: String,
    pub stack: Option<String>,
    pub request_id: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

/// Pagination parameters.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

impl PaginationParams {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).min(200)
    }
    pub fn offset(&self) -> i64 {
        self.offset.unwrap_or(0)
    }
}

/// Query filters for listing errors.
#[derive(Debug, Deserialize)]
pub struct ErrorQueryParams {
    pub project_id: Option<String>,
    pub release: Option<String>,
    pub environment: Option<String>,
    pub source: Option<String>,
    pub route: Option<String>,
    pub status: Option<String>,
    pub since: Option<i64>,
    pub until: Option<i64>,
    pub sort: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Health check response.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub db_ok: bool,
    pub buffer_usage: f64,
}

/// Stats overview.
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    pub total_errors: i64,
    pub unresolved_errors: i64,
    pub total_events_24h: i64,
    pub top_routes: Vec<RouteErrorCount>,
}

#[derive(Debug, Serialize)]
pub struct RouteErrorCount {
    pub route: String,
    pub count: i64,
}

/// Hourly count bucket for trend charts.
#[derive(Debug, Serialize)]
pub struct HourlyCount {
    pub hour_bucket: i64,
    pub count: i64,
}

/// Status change audit entry.
#[derive(Debug, Serialize)]
pub struct StatusChange {
    pub id: i64,
    pub project_id: String,
    pub fingerprint: String,
    pub old_status: String,
    pub new_status: String,
    pub changed_by: Option<String>,
    pub changed_at: i64,
}

/// Query params for trend endpoints.
#[derive(Debug, Deserialize)]
pub struct TrendQueryParams {
    pub project_id: Option<String>,
    pub hours: Option<i64>,
}
