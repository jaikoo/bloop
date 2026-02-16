use serde::Deserialize;
use std::collections::HashMap;
use std::sync::RwLock;

/// Per-token pricing entry for a model.
#[derive(Debug, Clone)]
pub struct ModelPrice {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub provider: Option<String>,
}

/// Deserialization type for LiteLLM's pricing JSON entries.
#[derive(Debug, Deserialize)]
struct LiteLlmEntry {
    input_cost_per_token: Option<f64>,
    output_cost_per_token: Option<f64>,
    litellm_provider: Option<String>,
}

/// In-memory pricing table with bundled defaults and runtime refresh.
pub struct PricingTable {
    prices: RwLock<HashMap<String, ModelPrice>>,
    overrides: RwLock<HashMap<String, ModelPrice>>,
}

impl Default for PricingTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PricingTable {
    /// Create a new PricingTable from the bundled LiteLLM pricing data.
    pub fn new() -> Self {
        let bundled = include_bytes!("../../data/model_prices.json");
        let prices = Self::parse_litellm_json(bundled);
        tracing::info!(models = prices.len(), "loaded bundled model pricing");
        Self {
            prices: RwLock::new(prices),
            overrides: RwLock::new(HashMap::new()),
        }
    }

    /// Parse LiteLLM JSON into a HashMap of model -> ModelPrice.
    fn parse_litellm_json(data: &[u8]) -> HashMap<String, ModelPrice> {
        let raw: HashMap<String, LiteLlmEntry> = match serde_json::from_slice(data) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "failed to parse pricing JSON");
                return HashMap::new();
            }
        };

        let mut prices = HashMap::with_capacity(raw.len());
        for (model, entry) in raw {
            // Skip entries without pricing data (e.g. "sample_spec")
            if let (Some(input), Some(output)) =
                (entry.input_cost_per_token, entry.output_cost_per_token)
            {
                prices.insert(
                    model,
                    ModelPrice {
                        input_cost_per_token: input,
                        output_cost_per_token: output,
                        provider: entry.litellm_provider,
                    },
                );
            }
        }
        prices
    }

    /// Refresh prices from raw JSON bytes (e.g. fetched from GitHub).
    pub fn refresh(&self, data: &[u8]) {
        let new_prices = Self::parse_litellm_json(data);
        if new_prices.is_empty() {
            tracing::warn!("refresh returned empty pricing data, keeping existing");
            return;
        }
        tracing::info!(models = new_prices.len(), "refreshed model pricing");
        *self.prices.write().unwrap_or_else(|e| e.into_inner()) = new_prices;
    }

    /// Set a custom override price for a model.
    pub fn set_override(&self, model: String, price: ModelPrice) {
        self.overrides
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(model, price);
    }

    /// Load overrides from the database (called at startup and after PUT).
    pub fn load_overrides(&self, overrides: Vec<(String, ModelPrice)>) {
        let mut map = self.overrides.write().unwrap_or_else(|e| e.into_inner());
        map.clear();
        for (model, price) in overrides {
            map.insert(model, price);
        }
    }

    /// Calculate cost in microdollars for a given model and token counts.
    /// Returns 0 if the model is not found in pricing data.
    pub fn calculate_cost_micros(&self, model: &str, input_tokens: i64, output_tokens: i64) -> i64 {
        if let Some(price) = self.lookup(model) {
            let cost_dollars = (input_tokens as f64) * price.input_cost_per_token
                + (output_tokens as f64) * price.output_cost_per_token;
            (cost_dollars * 1_000_000.0).round() as i64
        } else {
            0
        }
    }

    /// Look up pricing for a model, trying overrides first, then base prices.
    /// Uses a multi-step matching strategy:
    /// 1. Exact match
    /// 2. Provider-prefixed: "{provider}/{model}"
    /// 3. Strip date suffix: "gpt-4o-2024-08-06" -> "gpt-4o"
    pub fn lookup(&self, model: &str) -> Option<ModelPrice> {
        // 1. Check overrides first (exact match only)
        {
            let overrides = self.overrides.read().unwrap_or_else(|e| e.into_inner());
            if let Some(price) = overrides.get(model) {
                return Some(price.clone());
            }
        }

        let prices = self.prices.read().unwrap_or_else(|e| e.into_inner());

        // 1. Exact match
        if let Some(price) = prices.get(model) {
            return Some(price.clone());
        }

        // 2. Try common provider prefixes
        for prefix in &[
            "openai/",
            "anthropic/",
            "google/",
            "azure/",
            "cohere/",
            "mistral/",
        ] {
            let prefixed = format!("{prefix}{model}");
            if let Some(price) = prices.get(&prefixed) {
                return Some(price.clone());
            }
        }

        // 3. Strip date suffix (e.g. "gpt-4o-2024-08-06" -> "gpt-4o")
        if let Some(base) = strip_date_suffix(model) {
            if let Some(price) = prices.get(base) {
                return Some(price.clone());
            }
            // Also try provider-prefixed with stripped name
            for prefix in &["openai/", "anthropic/", "google/", "azure/"] {
                let prefixed = format!("{prefix}{base}");
                if let Some(price) = prices.get(&prefixed) {
                    return Some(price.clone());
                }
            }
        }

        None
    }

    /// Get pricing info for a model (for the GET /v1/llm/pricing endpoint).
    pub fn get_pricing_info(&self, model: &str) -> Option<ModelPrice> {
        self.lookup(model)
    }

    /// Get the number of models in the pricing table.
    pub fn model_count(&self) -> usize {
        self.prices.read().unwrap_or_else(|e| e.into_inner()).len()
    }
}

/// Strip a trailing date suffix like "-2024-08-06" from a model name.
/// Returns the base name if a date suffix is found, None otherwise.
fn strip_date_suffix(model: &str) -> Option<&str> {
    // Match pattern: -YYYY-MM-DD at end of string
    let bytes = model.as_bytes();
    if bytes.len() < 11 {
        return None;
    }
    let suffix_start = bytes.len() - 11;
    // Check: -DDDD-DD-DD
    if bytes[suffix_start] == b'-'
        && bytes[suffix_start + 5] == b'-'
        && bytes[suffix_start + 8] == b'-'
        && bytes[suffix_start + 1..suffix_start + 5]
            .iter()
            .all(|b| b.is_ascii_digit())
        && bytes[suffix_start + 6..suffix_start + 8]
            .iter()
            .all(|b| b.is_ascii_digit())
        && bytes[suffix_start + 9..suffix_start + 11]
            .iter()
            .all(|b| b.is_ascii_digit())
    {
        Some(&model[..suffix_start])
    } else {
        None
    }
}

/// Background task: periodically refresh pricing from LiteLLM GitHub.
pub async fn pricing_refresh_loop(
    pricing: std::sync::Arc<PricingTable>,
    interval_secs: u64,
    url: String,
) {
    let client = reqwest::Client::new();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
    // Skip first tick (we already loaded bundled data)
    interval.tick().await;

    loop {
        interval.tick().await;
        tracing::info!("refreshing model pricing from {url}");
        match client.get(&url).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    match resp.bytes().await {
                        Ok(bytes) => pricing.refresh(&bytes),
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to read pricing response body")
                        }
                    }
                } else {
                    tracing::warn!(status = %resp.status(), "pricing refresh got non-200 response");
                }
            }
            Err(e) => tracing::warn!(error = %e, "pricing refresh request failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_date_suffix() {
        assert_eq!(strip_date_suffix("gpt-4o-2024-08-06"), Some("gpt-4o"));
        assert_eq!(strip_date_suffix("claude-3-5-sonnet-20241022"), None);
        assert_eq!(strip_date_suffix("gpt-4o"), None);
        assert_eq!(strip_date_suffix(""), None);
    }

    #[test]
    fn test_pricing_table_bundled_data() {
        let table = PricingTable::new();
        assert!(table.model_count() > 100);

        // gpt-4o should be in bundled data
        let price = table.lookup("gpt-4o").expect("gpt-4o should exist");
        assert!(price.input_cost_per_token > 0.0);
        assert!(price.output_cost_per_token > 0.0);
    }

    #[test]
    fn test_calculate_cost_micros() {
        let table = PricingTable::new();

        // gpt-4o: input=$2.50/M, output=$10.00/M
        // 500 input + 100 output = (500 * 2.5e-6 + 100 * 10e-6) * 1e6 = 1250 + 1000 = 2250
        let cost = table.calculate_cost_micros("gpt-4o", 500, 100);
        assert!(cost > 0, "cost should be positive for known model");

        // Unknown model
        let cost = table.calculate_cost_micros("totally-fake-model-xyz", 500, 100);
        assert_eq!(cost, 0, "cost should be 0 for unknown model");
    }

    #[test]
    fn test_override_takes_precedence() {
        let table = PricingTable::new();

        table.set_override(
            "gpt-4o".to_string(),
            ModelPrice {
                input_cost_per_token: 0.001,
                output_cost_per_token: 0.002,
                provider: Some("openai".to_string()),
            },
        );

        let price = table.lookup("gpt-4o").unwrap();
        assert_eq!(price.input_cost_per_token, 0.001);
        assert_eq!(price.output_cost_per_token, 0.002);
    }

    #[test]
    fn test_zero_tokens_zero_cost() {
        let table = PricingTable::new();
        let cost = table.calculate_cost_micros("gpt-4o", 0, 0);
        assert_eq!(cost, 0);
    }
}
