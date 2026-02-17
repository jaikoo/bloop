use serde::{Deserialize, Serialize};

// ── Span Types ──

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SpanType {
    Generation,
    Tool,
    Retrieval,
    Custom,
}

#[allow(dead_code)]
impl SpanType {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpanType::Generation => "generation",
            SpanType::Tool => "tool",
            SpanType::Retrieval => "retrieval",
            SpanType::Custom => "custom",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "generation" => SpanType::Generation,
            "tool" => SpanType::Tool,
            "retrieval" => SpanType::Retrieval,
            _ => SpanType::Custom,
        }
    }
}

// ── Content Storage Policy ──

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ContentStorage {
    None,
    MetadataOnly,
    Full,
}

impl ContentStorage {
    pub fn as_str(&self) -> &'static str {
        match self {
            ContentStorage::None => "none",
            ContentStorage::MetadataOnly => "metadata_only",
            ContentStorage::Full => "full",
        }
    }

    pub fn from_str_loose(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "full" => ContentStorage::Full,
            "metadata_only" => ContentStorage::MetadataOnly,
            _ => ContentStorage::None,
        }
    }
}

// ── Ingest Types ──

#[derive(Debug, Deserialize)]
pub struct IngestSpan {
    pub id: Option<String>,
    pub parent_span_id: Option<String>,
    #[serde(default = "default_span_type")]
    pub span_type: String,
    #[serde(default)]
    pub name: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    /// Cost in dollars (float) — converted to microdollars (integer) at ingest
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub latency_ms: i64,
    pub time_to_first_token_ms: Option<i64>,
    #[serde(default = "default_status")]
    pub status: String,
    pub error_message: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

fn default_span_type() -> String {
    "generation".to_string()
}

fn default_status() -> String {
    "ok".to_string()
}

#[derive(Debug, Deserialize)]
pub struct IngestTrace {
    pub id: Option<String>,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_trace_status")]
    pub status: String,
    pub input: Option<String>,
    pub output: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub prompt_name: Option<String>,
    pub prompt_version: Option<String>,
    #[serde(default)]
    pub spans: Vec<IngestSpan>,
}

fn default_trace_status() -> String {
    "completed".to_string()
}

#[derive(Debug, Deserialize)]
pub struct BatchTracePayload {
    pub traces: Vec<IngestTrace>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTrace {
    pub status: Option<String>,
    pub output: Option<String>,
    pub ended_at: Option<i64>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cost: Option<f64>,
}

// ── Processed (internal) types ──

#[derive(Debug, Clone)]
pub struct ProcessedSpan {
    pub id: String,
    pub trace_id: String,
    pub parent_span_id: Option<String>,
    pub span_type: String,
    pub name: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cost_micros: i64,
    pub latency_ms: i64,
    pub time_to_first_token_ms: Option<i64>,
    pub status: String,
    pub error_message: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub metadata: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ProcessedTrace {
    pub project_id: String,
    pub id: String,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub name: String,
    pub status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cost_micros: i64,
    pub input: Option<String>,
    pub output: Option<String>,
    pub metadata: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub created_at: i64,
    pub prompt_name: Option<String>,
    pub prompt_version: Option<String>,
    pub spans: Vec<ProcessedSpan>,
}

/// Convert dollars (float) to microdollars (integer).
/// 1 dollar = 1,000,000 micros.
pub fn dollars_to_micros(dollars: f64) -> i64 {
    (dollars * 1_000_000.0).round() as i64
}

// ── Query Response Types ──

#[derive(Debug, Serialize, Deserialize)]
pub struct LlmQueryParams {
    pub project_id: Option<String>,
    pub hours: Option<i64>,
    pub limit: Option<i64>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub session_id: Option<String>,
    pub offset: Option<i64>,
    pub prompt_name: Option<String>,
    pub prompt_version: Option<String>,
    pub user_id: Option<String>,
    pub status: Option<String>,
    pub min_cost_micros: Option<i64>,
    pub min_latency_ms: Option<i64>,
    pub search: Option<String>,
    pub has_error: Option<bool>,
    pub sort: Option<String>,
}

impl LlmQueryParams {
    pub fn hours(&self) -> i64 {
        self.hours.unwrap_or(24).clamp(1, 720)
    }

    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }

    pub fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }

    /// Return the sort key, defaulting to "newest".
    pub fn sort_key(&self) -> &str {
        self.sort.as_deref().unwrap_or("newest")
    }
}

#[derive(Debug, Serialize)]
pub struct OverviewResponse {
    pub total_traces: i64,
    pub total_spans: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub total_tokens: i64,
    pub total_cost_micros: i64,
    pub error_count: i64,
    pub error_rate: f64,
    pub window_hours: i64,
}

#[derive(Debug, Serialize)]
pub struct UsageEntry {
    pub hour_bucket: i64,
    pub model: String,
    pub provider: String,
    pub span_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cost_micros: i64,
}

#[derive(Debug, Serialize)]
pub struct UsageResponse {
    pub usage: Vec<UsageEntry>,
    pub window_hours: i64,
}

#[derive(Debug, Serialize)]
pub struct LatencyEntry {
    pub model: String,
    pub p50_ms: f64,
    pub p90_ms: f64,
    pub p99_ms: f64,
    pub avg_ttft_ms: f64,
    pub span_count: i64,
}

#[derive(Debug, Serialize)]
pub struct LatencyResponse {
    pub latency: Vec<LatencyEntry>,
    pub window_hours: i64,
}

#[derive(Debug, Serialize)]
pub struct ModelEntry {
    pub model: String,
    pub provider: String,
    pub span_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cost_micros: i64,
    pub error_count: i64,
    pub error_rate: f64,
    pub avg_latency_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct ModelsResponse {
    pub models: Vec<ModelEntry>,
    pub window_hours: i64,
}

#[derive(Debug, Serialize)]
pub struct TraceListEntry {
    pub id: String,
    pub session_id: Option<String>,
    pub name: String,
    pub status: String,
    pub total_tokens: i64,
    pub cost_micros: i64,
    pub span_count: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub prompt_name: Option<String>,
    pub prompt_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TraceListResponse {
    pub traces: Vec<TraceListEntry>,
    pub total: i64,
    pub window_hours: i64,
}

#[derive(Debug, Serialize)]
pub struct SpanDetail {
    pub id: String,
    pub parent_span_id: Option<String>,
    pub span_type: String,
    pub name: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cost_micros: i64,
    pub latency_ms: i64,
    pub time_to_first_token_ms: Option<i64>,
    pub status: String,
    pub error_message: Option<String>,
    pub input: Option<String>,
    pub output: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TraceDetailResponse {
    pub id: String,
    pub project_id: String,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub name: String,
    pub status: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub cost_micros: i64,
    pub input: Option<String>,
    pub output: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub prompt_name: Option<String>,
    pub prompt_version: Option<String>,
    pub spans: Vec<SpanDetail>,
}

#[derive(Debug, Serialize)]
pub struct PromptEntry {
    pub name: String,
    pub version_count: i64,
    pub total_traces: i64,
    pub avg_cost_micros: i64,
    pub error_rate: f64,
}

#[derive(Debug, Serialize)]
pub struct PromptsResponse {
    pub prompts: Vec<PromptEntry>,
    pub window_hours: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SettingsResponse {
    pub project_id: String,
    pub content_storage: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettings {
    pub content_storage: String,
}

// ── Score Types ──

#[derive(Debug, Deserialize)]
pub struct ScoreInput {
    pub name: String,
    pub value: f64,
    pub source: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScoreResponse {
    pub name: String,
    pub value: f64,
    pub source: String,
    pub comment: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct ScoresListResponse {
    pub scores: Vec<ScoreResponse>,
}

#[derive(Debug, Serialize)]
pub struct ScoreSummary {
    pub name: String,
    pub count: i64,
    pub avg: f64,
    pub p50: f64,
    pub p10: f64,
    pub p90: f64,
    pub distribution: std::collections::HashMap<String, i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ScoreSummaryParams {
    pub project_id: Option<String>,
    pub name: Option<String>,
    pub hours: Option<i64>,
}

// ── Session Types ──

#[derive(Debug, Serialize)]
pub struct SessionEntry {
    pub session_id: String,
    pub user_id: Option<String>,
    pub trace_count: i64,
    pub total_cost_micros: i64,
    pub total_tokens: i64,
    pub first_trace_at: i64,
    pub last_trace_at: i64,
    pub error_count: i64,
}

#[derive(Debug, Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionEntry>,
    pub total: i64,
    pub window_hours: i64,
}

#[derive(Debug, Serialize)]
pub struct SessionDetailResponse {
    pub session_id: String,
    pub user_id: Option<String>,
    pub traces: Vec<TraceListEntry>,
    pub total_cost_micros: i64,
    pub total_tokens: i64,
}

// ── Tool Types ──

#[derive(Debug, Serialize)]
pub struct ToolEntry {
    pub name: String,
    pub call_count: i64,
    pub error_count: i64,
    pub error_rate: f64,
    pub avg_latency_ms: f64,
    pub p50_latency_ms: f64,
    pub p99_latency_ms: f64,
    pub total_cost_micros: i64,
}

#[derive(Debug, Serialize)]
pub struct ToolsResponse {
    pub tools: Vec<ToolEntry>,
    pub window_hours: i64,
}

// ── Feedback Types ──

#[derive(Debug, Deserialize)]
pub struct FeedbackInput {
    pub value: i64,
    pub user_id: Option<String>,
    pub comment: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FeedbackResponse {
    pub trace_id: String,
    pub user_id: String,
    pub value: i64,
    pub comment: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Serialize)]
pub struct FeedbackListResponse {
    pub feedback: Vec<FeedbackResponse>,
}

#[derive(Debug, Serialize)]
pub struct FeedbackSummary {
    pub total: i64,
    pub positive: i64,
    pub negative: i64,
    pub positive_rate: f64,
    pub window_hours: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeedbackSummaryParams {
    pub project_id: Option<String>,
    pub hours: Option<i64>,
}

// ── Prompt Version / Comparison Types ──

#[derive(Debug, Serialize)]
pub struct PromptVersionEntry {
    pub version: String,
    pub trace_count: i64,
    pub avg_cost_micros: i64,
    pub avg_latency_ms: f64,
    pub error_rate: f64,
    pub avg_input_tokens: f64,
    pub avg_output_tokens: f64,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Serialize)]
pub struct PromptVersionsResponse {
    pub name: String,
    pub versions: Vec<PromptVersionEntry>,
    pub window_hours: i64,
}

#[derive(Debug, Deserialize)]
pub struct PromptCompareParams {
    pub project_id: Option<String>,
    pub hours: Option<i64>,
    pub v1: String,
    pub v2: String,
}

#[derive(Debug, Serialize)]
pub struct PromptComparisonResponse {
    pub name: String,
    pub v1: PromptVersionEntry,
    pub v2: PromptVersionEntry,
}

// ── Budget Types ──

#[derive(Debug, Deserialize)]
pub struct BudgetInput {
    pub monthly_budget_micros: i64,
    pub alert_threshold_pct: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct BudgetResponse {
    pub project_id: String,
    pub monthly_budget_micros: i64,
    pub alert_threshold_pct: i64,
    pub current_month_spend_micros: i64,
    pub budget_used_pct: f64,
    pub forecast_month_end_micros: i64,
    pub forecast_over_budget: bool,
    pub days_elapsed: i64,
    pub days_remaining: i64,
}

// ── RAG Types ──

#[derive(Debug, Serialize)]
pub struct RagSourceEntry {
    pub retrieval_name: String,
    pub source: String,
    pub call_count: i64,
    pub avg_latency_ms: f64,
    pub p50_latency_ms: f64,
    pub p95_latency_ms: f64,
    pub error_count: i64,
    pub error_rate: f64,
    pub avg_chunks_retrieved: f64,
    pub avg_chunks_used: f64,
    pub avg_context_tokens: f64,
    pub avg_context_utilization_pct: f64,
}

#[derive(Debug, Serialize)]
pub struct RagMetricsEntry {
    pub hour_bucket: i64,
    pub retrieval_count: i64,
    pub avg_latency_ms: f64,
    pub error_count: i64,
    pub avg_chunks_retrieved: f64,
    pub avg_context_tokens: f64,
    pub avg_context_utilization_pct: f64,
    pub avg_top_k: f64,
}

#[derive(Debug, Serialize)]
pub struct RagRelevanceSummary {
    pub total_retrievals: i64,
    pub avg_top_relevance: f64,
    pub min_top_relevance: f64,
    pub max_top_relevance: f64,
    pub avg_chunks_retrieved: f64,
    pub avg_chunks_used: f64,
    pub chunk_utilization_pct: f64,
}

#[derive(Debug, Serialize)]
pub struct RagResponse {
    pub sources: Vec<RagSourceEntry>,
    pub metrics: Vec<RagMetricsEntry>,
    pub relevance: RagRelevanceSummary,
    pub window_hours: i64,
}
