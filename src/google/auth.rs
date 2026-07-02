//! OAuth 2.0 installed-app flow via `yup-oauth2`.
//!
//! Builds an authenticator from the client id/secret stored in config (no
//! downloaded JSON needed) and persists the refresh token to the XDG data dir
//! so subsequent runs are non-interactive.

use anyhow::{Context, Result};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::connect::HttpConnector;
use yup_oauth2::authenticator::Authenticator;
use yup_oauth2::{ApplicationSecret, InstalledFlowAuthenticator, InstalledFlowReturnMethod};

use crate::config::{token_path, Config};

// Scopes are applied per-call by the generated `google-calendar3` methods
// (list uses readonly, insert uses the full calendar scope), so we don't set
// them on the authenticator here.

/// The concrete authenticator type used throughout (rustls connector).
pub type Auth = Authenticator<HttpsConnector<HttpConnector>>;

/// Build (and, on first run, interactively authorize) the OAuth authenticator.
pub async fn build_authenticator(cfg: &Config) -> Result<Auth> {
    anyhow::ensure!(
        cfg.has_credentials(),
        "missing client_id/client_secret in config — complete the Google Cloud setup first"
    );

    let secret = ApplicationSecret {
        client_id: cfg.client_id.clone(),
        client_secret: cfg.client_secret.clone(),
        auth_uri: "https://accounts.google.com/o/oauth2/auth".to_string(),
        token_uri: "https://oauth2.googleapis.com/token".to_string(),
        // HTTPRedirect spins up a transient localhost listener; this value is a
        // placeholder the crate substitutes with the real localhost port.
        redirect_uris: vec!["http://127.0.0.1".to_string()],
        ..Default::default()
    };

    let token_file = token_path()?;
    if let Some(parent) = token_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating token dir {}", parent.display()))?;
    }

    let auth = InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
        .persist_tokens_to_disk(token_file)
        .build()
        .await
        .context("building OAuth authenticator")?;

    Ok(auth)
}
