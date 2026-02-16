use config::{Config, Environment, File};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub ingest: IngestConfig,
    pub pipeline: PipelineConfig,
    pub retention: RetentionConfig,
    pub auth: AuthConfig,
    #[allow(dead_code)]
    pub rate_limit: RateLimitConfig,
    pub alerting: AlertingConfig,
    #[serde(default)]
    pub smtp: SmtpConfig,
    #[serde(default)]
    #[allow(dead_code)]
    pub analytics: AnalyticsConfig,
    #[serde(default)]
    #[allow(dead_code)]
    pub llm_tracing: LlmTracingConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    pub path: PathBuf,
    #[allow(dead_code)]
    pub pool_size: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct IngestConfig {
    pub max_payload_bytes: usize,
    pub max_stack_bytes: usize,
    pub max_metadata_bytes: usize,
    pub max_message_bytes: usize,
    pub max_batch_size: usize,
    pub channel_capacity: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PipelineConfig {
    pub flush_interval_secs: u64,
    pub flush_batch_size: usize,
    pub sample_reservoir_size: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RetentionConfig {
    pub raw_events_days: u64,
    pub prune_interval_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthConfig {
    pub hmac_secret: String,
    #[serde(default = "default_rp_id")]
    pub rp_id: String,
    #[serde(default = "default_rp_origin")]
    pub rp_origin: String,
    #[serde(default = "default_session_ttl")]
    pub session_ttl_secs: u64,
}

fn default_rp_id() -> String {
    "localhost".to_string()
}

fn default_rp_origin() -> String {
    "http://localhost:5332".to_string()
}

fn default_session_ttl() -> u64 {
    604800 // 7 days
}

#[derive(Debug, Deserialize, Clone)]
pub struct RateLimitConfig {
    pub per_second: u64,
    pub burst_size: u32,
    #[serde(default = "default_auth_per_second")]
    pub auth_per_second: u64,
    #[serde(default = "default_auth_burst_size")]
    pub auth_burst_size: u32,
}

fn default_auth_per_second() -> u64 {
    5
}
fn default_auth_burst_size() -> u32 {
    10
}

#[derive(Debug, Deserialize, Clone)]
pub struct AlertingConfig {
    pub cooldown_secs: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct AnalyticsConfig {
    #[serde(default = "default_analytics_enabled")]
    pub enabled: bool,
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_zscore_threshold")]
    pub zscore_threshold: f64,
}

impl Default for AnalyticsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cache_ttl_secs: 60,
            zscore_threshold: 2.5,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct SmtpConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_smtp_host")]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default = "default_smtp_from")]
    pub from: String,
    #[serde(default = "default_smtp_starttls")]
    pub starttls: bool,
}

impl Default for SmtpConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "smtp.example.com".to_string(),
            port: 587,
            username: String::new(),
            password: String::new(),
            from: "bloop@example.com".to_string(),
            starttls: true,
        }
    }
}

fn default_smtp_host() -> String {
    "smtp.example.com".to_string()
}
fn default_smtp_port() -> u16 {
    587
}
fn default_smtp_from() -> String {
    "bloop@example.com".to_string()
}
fn default_smtp_starttls() -> bool {
    true
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
pub struct LlmTracingConfig {
    #[serde(default = "default_llm_enabled")]
    pub enabled: bool,
    #[serde(default = "default_llm_channel_capacity")]
    pub channel_capacity: usize,
    #[serde(default = "default_llm_flush_interval")]
    pub flush_interval_secs: u64,
    #[serde(default = "default_llm_flush_batch_size")]
    pub flush_batch_size: usize,
    #[serde(default = "default_llm_max_spans")]
    pub max_spans_per_trace: usize,
    #[serde(default = "default_llm_max_batch")]
    pub max_batch_size: usize,
    #[serde(default = "default_llm_content_storage")]
    pub default_content_storage: String,
    #[serde(default = "default_llm_cache_ttl")]
    pub cache_ttl_secs: u64,
    #[serde(default = "default_llm_pricing_refresh_interval")]
    pub pricing_refresh_interval_secs: u64,
    #[serde(default = "default_llm_pricing_url")]
    pub pricing_url: String,
}

impl Default for LlmTracingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            channel_capacity: 4096,
            flush_interval_secs: 2,
            flush_batch_size: 200,
            max_spans_per_trace: 100,
            max_batch_size: 50,
            default_content_storage: "none".to_string(),
            cache_ttl_secs: 30,
            pricing_refresh_interval_secs: 3600,
            pricing_url: default_llm_pricing_url(),
        }
    }
}

fn default_llm_enabled() -> bool {
    true
}
fn default_llm_channel_capacity() -> usize {
    4096
}
fn default_llm_flush_interval() -> u64 {
    2
}
fn default_llm_flush_batch_size() -> usize {
    200
}
fn default_llm_max_spans() -> usize {
    100
}
fn default_llm_max_batch() -> usize {
    50
}
fn default_llm_content_storage() -> String {
    "none".to_string()
}
fn default_llm_cache_ttl() -> u64 {
    30
}
fn default_llm_pricing_refresh_interval() -> u64 {
    3600
}
fn default_llm_pricing_url() -> String {
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json"
        .to_string()
}

fn default_analytics_enabled() -> bool {
    true
}
fn default_cache_ttl() -> u64 {
    60
}
fn default_zscore_threshold() -> f64 {
    2.5
}

impl AppConfig {
    /// Validate configuration for security requirements.
    pub fn validate(&self) -> Result<(), String> {
        if self.auth.hmac_secret.is_empty() || self.auth.hmac_secret == "change-me-in-production" {
            return Err("auth.hmac_secret must be set to a strong, unique value. \
                 Set it in config.toml or via BLOOP__AUTH__HMAC_SECRET env var."
                .to_string());
        }
        if self.auth.hmac_secret.len() < 32 {
            return Err("auth.hmac_secret must be at least 32 characters. \
                 Set a longer secret in config.toml or via BLOOP__AUTH__HMAC_SECRET env var."
                .to_string());
        }
        Ok(())
    }

    pub fn load(config_path: Option<&str>) -> Result<Self, config::ConfigError> {
        let mut builder = Config::builder();

        // Load from config file
        let path = config_path.unwrap_or("config.toml");
        builder = builder.add_source(File::with_name(path).required(false));

        // Overlay with environment variables (BLOOP__SERVER__PORT=3001, etc.)
        builder = builder.add_source(
            Environment::with_prefix("BLOOP")
                .separator("__")
                .try_parsing(true),
        );

        builder.build()?.try_deserialize()
    }
}
