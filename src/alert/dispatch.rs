use serde_json::json;

/// Dispatches alert notifications to Slack webhooks, generic webhooks, and email.
pub struct AlertDispatcher {
    client: reqwest::Client,
    slack_webhook_url: Option<String>,
    generic_webhook_url: Option<String>,
    smtp_transport: Option<lettre::AsyncSmtpTransport<lettre::Tokio1Executor>>,
    smtp_from: Option<String>,
}

/// Validate that a webhook URL is safe to call (HTTPS, no private IPs).
pub fn validate_webhook_url(url: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid URL: {e}"))?;
    match parsed.scheme() {
        "https" => {}
        "http" => {
            tracing::warn!(url = url, "webhook URL uses HTTP; HTTPS is recommended");
        }
        scheme => return Err(format!("unsupported scheme: {scheme}")),
    }
    if let Some(host) = parsed.host_str() {
        if host == "localhost"
            || host == "127.0.0.1"
            || host == "::1"
            || host.starts_with("10.")
            || host.starts_with("192.168.")
            || host.starts_with("169.254.")
            || (host.starts_with("172.")
                && host
                    .split('.')
                    .nth(1)
                    .and_then(|s| s.parse::<u8>().ok())
                    .is_some_and(|n| (16..=31).contains(&n)))
        {
            return Err(format!(
                "webhook URL must not point to private/loopback address: {host}"
            ));
        }
    }
    Ok(())
}

/// Channel configuration for per-rule notification routing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChannelConfig {
    Slack { webhook_url: String },
    Webhook { url: String },
    Email { to: String },
}

impl AlertDispatcher {
    pub fn new(
        slack_webhook_url: Option<String>,
        generic_webhook_url: Option<String>,
        smtp_config: Option<crate::config::SmtpConfig>,
    ) -> Self {
        // Validate webhook URLs at startup
        if let Some(ref url) = slack_webhook_url {
            if let Err(e) = validate_webhook_url(url) {
                tracing::warn!(error = %e, "slack webhook URL validation warning");
            }
        }
        if let Some(ref url) = generic_webhook_url {
            if let Err(e) = validate_webhook_url(url) {
                tracing::warn!(error = %e, "generic webhook URL validation warning");
            }
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build HTTP client");

        let (smtp_transport, smtp_from) = match smtp_config {
            Some(ref cfg) if cfg.enabled => {
                let builder = if cfg.starttls {
                    lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::starttls_relay(&cfg.host)
                } else {
                    lettre::AsyncSmtpTransport::<lettre::Tokio1Executor>::relay(&cfg.host)
                };
                match builder {
                    Ok(b) => {
                        let transport = b
                            .port(cfg.port)
                            .credentials(lettre::transport::smtp::authentication::Credentials::new(
                                cfg.username.clone(),
                                cfg.password.clone(),
                            ))
                            .build();
                        tracing::info!(host = %cfg.host, port = cfg.port, "SMTP transport initialized");
                        (Some(transport), Some(cfg.from.clone()))
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to create SMTP transport");
                        (None, None)
                    }
                }
            }
            _ => (None, None),
        };

        Self {
            client,
            slack_webhook_url,
            generic_webhook_url,
            smtp_transport,
            smtp_from,
        }
    }

    pub async fn dispatch(&self, rule_name: &str, message: &str) {
        if let Some(ref url) = self.slack_webhook_url {
            self.send_slack(url, rule_name, message).await;
        }
        if let Some(ref url) = self.generic_webhook_url {
            self.send_generic(url, rule_name, message).await;
        }
    }

    async fn send_slack(&self, url: &str, rule_name: &str, message: &str) {
        let payload = json!({
            "text": format!("*[bloop alert: {}]*\n{}", rule_name, message),
        });

        match self.client.post(url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(rule = rule_name, "slack alert sent");
            }
            Ok(resp) => {
                tracing::warn!(
                    rule = rule_name,
                    status = %resp.status(),
                    "slack alert failed"
                );
            }
            Err(e) => {
                tracing::error!(rule = rule_name, error = %e, "slack alert error");
            }
        }
    }

    async fn send_generic(&self, url: &str, rule_name: &str, message: &str) {
        let payload = json!({
            "rule": rule_name,
            "message": message,
            "timestamp": chrono::Utc::now().timestamp(),
            "source": "bloop",
        });

        match self.client.post(url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(rule = rule_name, "webhook alert sent");
            }
            Ok(resp) => {
                tracing::warn!(
                    rule = rule_name,
                    status = %resp.status(),
                    "webhook alert failed"
                );
            }
            Err(e) => {
                tracing::error!(rule = rule_name, error = %e, "webhook alert error");
            }
        }
    }

    async fn send_email(&self, to: &str, rule_name: &str, message: &str) {
        let transport = match &self.smtp_transport {
            Some(t) => t,
            None => {
                tracing::warn!(rule = rule_name, "email alert skipped: SMTP not configured");
                return;
            }
        };
        let from = match &self.smtp_from {
            Some(f) => f.as_str(),
            None => return,
        };

        let email = match lettre::Message::builder()
            .from(
                from.parse()
                    .unwrap_or_else(|_| "bloop@localhost".parse().unwrap()),
            )
            .to(match to.parse() {
                Ok(addr) => addr,
                Err(e) => {
                    tracing::error!(error = %e, to = to, "invalid email recipient");
                    return;
                }
            })
            .subject(format!("[bloop alert] {}", rule_name))
            .body(message.to_string())
        {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(error = %e, "failed to build email");
                return;
            }
        };

        use lettre::AsyncTransport;
        match transport.send(email).await {
            Ok(_) => tracing::info!(rule = rule_name, to = to, "email alert sent"),
            Err(e) => tracing::error!(rule = rule_name, error = %e, "email alert failed"),
        }
    }

    /// Dispatch alert to specific channels configured on a rule.
    pub async fn dispatch_to_channels(
        &self,
        rule_name: &str,
        message: &str,
        channels: &[ChannelConfig],
    ) {
        for channel in channels {
            match channel {
                ChannelConfig::Slack { webhook_url } => {
                    self.send_slack(webhook_url, rule_name, message).await;
                }
                ChannelConfig::Webhook { url, .. } => {
                    self.send_generic(url, rule_name, message).await;
                }
                ChannelConfig::Email { to } => {
                    self.send_email(to, rule_name, message).await;
                }
            }
        }
        // Also fall back to global webhooks if no channels configured
        if channels.is_empty() {
            self.dispatch(rule_name, message).await;
        }
    }
}
