use deadpool_sqlite::Pool;
use moka::sync::Cache;
use std::sync::Arc;
use std::time::Duration;
use url::Url;
use webauthn_rs::prelude::*;
use webauthn_rs::Webauthn;

use crate::config::AuthConfig;

/// Shared auth state for WebAuthn ceremonies and session management.
#[derive(Clone)]
pub struct AuthState {
    pub webauthn: Arc<Webauthn>,
    /// Temporary challenge state cache (registration/login in-flight).
    /// Key: random challenge ID, Value: serialized JSON of PasskeyRegistration or PasskeyAuthentication.
    pub challenge_cache: Cache<String, String>,
    pub pool: Pool,
    pub session_ttl_secs: u64,
}

impl AuthState {
    pub fn new(config: &AuthConfig, pool: Pool) -> Result<Self, WebauthnError> {
        let rp_origin = Url::parse(&config.rp_origin).expect("auth.rp_origin must be a valid URL");
        let builder = WebauthnBuilder::new(&config.rp_id, &rp_origin)?;
        let webauthn = Arc::new(builder.build()?);

        let challenge_cache = Cache::builder()
            .max_capacity(100)
            .time_to_live(Duration::from_secs(120))
            .build();

        Ok(Self {
            webauthn,
            challenge_cache,
            pool,
            session_ttl_secs: config.session_ttl_secs,
        })
    }
}
