use serde::Serialize;

/// Common query parameters for all analytics endpoints.
#[derive(Debug, serde::Deserialize)]
pub struct AnalyticsQueryParams {
    pub project_id: Option<String>,
    pub hours: Option<i64>,
    pub limit: Option<i64>,
    pub environment: Option<String>,
}

impl AnalyticsQueryParams {
    pub fn hours(&self) -> i64 {
        self.hours.unwrap_or(72).clamp(1, 720)
    }

    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(20).clamp(1, 100)
    }
}

// ── Spike Detection ──

#[derive(Debug, Serialize)]
pub struct SpikeEntry {
    pub fingerprint: String,
    pub error_type: String,
    pub message: String,
    pub hour_bucket: i64,
    pub count: i64,
    pub mean: f64,
    pub stddev: f64,
    pub zscore: f64,
}

#[derive(Debug, Serialize)]
pub struct SpikeResponse {
    pub spikes: Vec<SpikeEntry>,
    pub threshold: f64,
    pub window_hours: i64,
}

// ── Top Movers ──

#[derive(Debug, Serialize)]
pub struct MoverEntry {
    pub fingerprint: String,
    pub error_type: String,
    pub message: String,
    pub count_current: i64,
    pub count_previous: i64,
    pub delta: i64,
    pub delta_pct: f64,
}

#[derive(Debug, Serialize)]
pub struct MoversResponse {
    pub movers: Vec<MoverEntry>,
    pub window_hours: i64,
}

// ── Correlations ──

#[derive(Debug, Serialize)]
pub struct CorrelationPair {
    pub fingerprint_a: String,
    pub fingerprint_b: String,
    pub message_a: String,
    pub message_b: String,
    pub correlation: f64,
}

#[derive(Debug, Serialize)]
pub struct CorrelationsResponse {
    pub pairs: Vec<CorrelationPair>,
    pub window_hours: i64,
}

// ── Release Impact ──

#[derive(Debug, Serialize)]
pub struct ReleaseEntry {
    pub release: String,
    pub first_seen: i64,
    pub error_count: i64,
    pub unique_fingerprints: i64,
    pub new_fingerprints: i64,
    pub error_delta: i64,
    pub impact_score: f64,
}

#[derive(Debug, Serialize)]
pub struct ReleasesResponse {
    pub releases: Vec<ReleaseEntry>,
}

// ── Environment Breakdown ──

#[derive(Debug, Serialize)]
pub struct EnvironmentEntry {
    pub environment: String,
    pub total_count: i64,
    pub unique_errors: i64,
    pub pct_of_total: f64,
    pub p50_hourly: f64,
    pub p90_hourly: f64,
    pub p99_hourly: f64,
}

#[derive(Debug, Serialize)]
pub struct EnvironmentsResponse {
    pub environments: Vec<EnvironmentEntry>,
    pub total_count: i64,
}
