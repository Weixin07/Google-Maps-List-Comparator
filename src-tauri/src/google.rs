use std::sync::Arc;
use std::time::Duration as StdDuration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt;
use parking_lot::Mutex;
use reqwest::{Client, Url};
use rand::distributions::Alphanumeric;
use rand::{thread_rng, Rng};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::time::{sleep, timeout};

use crate::config::AppConfig;
use crate::errors::{AppError, AppResult};
use crate::sanitize_error_copy;
use crate::secrets::SecretVault;
use crate::telemetry::TelemetryClient;
use tracing::warn;

const TOKEN_ALIAS: &str = "google-oauth-token";
const DRIVE_KML_MIME: &str = "application/vnd.google-earth.kml+xml";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_WAIT_SECS: u64 = 5;
const DEFAULT_LOOPBACK_TIMEOUT_SECS: u64 = 180;
const LOOPBACK_PATH: &str = "/auth/callback";
const LOOPBACK_HOST: &str = "127.0.0.1";

const GOOGLE_SCOPES: &[&str] = &[
    "https://www.googleapis.com/auth/drive.readonly",
    "openid",
    "email",
    "profile",
];

#[derive(Clone)]
pub struct GoogleServices {
    http: Client,
    config: GoogleSettings,
    vault: SecretVault,
    telemetry: TelemetryClient,
    pending_auth: Arc<Mutex<Option<LoopbackSession>>>,
    refresh_state: Arc<RefreshState>,
}

#[derive(Clone)]
struct GoogleSettings {
    client_id: String,
    client_secret: String,
    device_code_endpoint: String,
    auth_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
    drive_api_base: String,
    scopes: String,
    picker_page_size: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceFlowState {
    pub device_code: String,
    pub user_code: String,
    pub verification_url: String,
    pub expires_at: DateTime<Utc>,
    pub interval_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoopbackFlowState {
    pub authorization_url: String,
    pub redirect_url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GoogleIdentity {
    pub email: String,
    pub name: Option<String>,
    pub picture: Option<String>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DriveFileMetadata {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub modified_time: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredGoogleToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub scope: String,
    pub token_type: String,
    #[serde(default)]
    pub next_refresh: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_failure: Option<String>,
}

impl StoredGoogleToken {
    fn new(
        access_token: String,
        refresh_token: Option<String>,
        expires_in: u64,
        scope: String,
        token_type: String,
    ) -> Self {
        let total = Duration::seconds(expires_in as i64);
        let buffer = Duration::seconds(expires_in.min(30) as i64);
        let expires_at = Utc::now() + total - buffer;
        Self {
            access_token,
            refresh_token,
            expires_at,
            scope,
            token_type,
            next_refresh: None,
            last_failure: None,
        }
    }

    fn is_expired(&self) -> bool {
        let buffer = Duration::minutes(5);
        Utc::now() + buffer >= self.expires_at
    }
}

impl From<DriveFileRaw> for DriveFileMetadata {
    fn from(value: DriveFileRaw) -> Self {
        Self {
            id: value.id,
            name: value.name,
            mime_type: value.mime_type,
            modified_time: value.modified_time,
            size: value.size.and_then(|s| s.parse().ok()),
        }
    }
}

struct LoopbackSession {
    state: String,
    code_verifier: String,
    redirect_url: String,
    receiver: oneshot::Receiver<Result<AuthCallback, AppError>>,
    expires_at: DateTime<Utc>,
}

struct AuthCallback {
    code: String,
    state: String,
}

struct RefreshState {
    next_refresh: Mutex<Option<DateTime<Utc>>>,
    refreshing: Mutex<bool>,
    last_failure: Mutex<Option<String>>,
}

impl GoogleServices {
    pub fn maybe_new(
        config: &AppConfig,
        vault: &SecretVault,
        telemetry: TelemetryClient,
    ) -> AppResult<Option<Self>> {
        let (client_id, client_secret) = match (
            config.google_oauth_client_id.clone(),
            config.google_oauth_client_secret.clone(),
        ) {
            (Some(id), Some(secret)) => (id, secret),
            _ => return Ok(None),
        };

        let http = Client::builder()
            .user_agent("google-maps-list-comparator/0.1.0")
            .build()?;

        let refresh_state = Arc::new(RefreshState {
            next_refresh: Mutex::new(None),
            refreshing: Mutex::new(false),
            last_failure: Mutex::new(None),
        });

        let instance = Self {
            http,
            config: GoogleSettings {
                client_id,
                client_secret,
                device_code_endpoint: config.google_device_code_endpoint.clone(),
                auth_endpoint: config.google_auth_endpoint.clone(),
                token_endpoint: config.google_token_endpoint.clone(),
                userinfo_endpoint: config.google_userinfo_endpoint.clone(),
                drive_api_base: config
                    .google_drive_api_base
                    .trim_end_matches('/')
                    .to_string(),
                scopes: GOOGLE_SCOPES.join(" "),
                picker_page_size: config.google_drive_picker_page_size,
            },
            vault: vault.clone(),
            pending_auth: Arc::new(Mutex::new(None)),
            telemetry,
            refresh_state: Arc::clone(&refresh_state),
        };

        instance.restore_refresh_state();

        let refresher = instance.clone();
        tokio::spawn(async move {
            refresher.run_refresh_loop().await;
        });

        Ok(Some(instance))
    }

    pub fn picker_page_size(&self) -> usize {
        self.config.picker_page_size
    }

    pub async fn start_device_flow(&self) -> AppResult<DeviceFlowState> {
        let response = self
            .http
            .post(&self.config.device_code_endpoint)
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", self.config.scopes.as_str()),
            ])
            .send()
            .await?
            .error_for_status()?;

        let device: DeviceCodeResponse = response.json().await?;
        let expires_at = Utc::now() + Duration::seconds(device.expires_in as i64);
        let verification_url = device
            .verification_uri_complete
            .or(device.verification_url.clone())
            .or(device.verification_uri.clone())
            .ok_or_else(|| AppError::Config("Google response missing verification URL".into()))?;

        Ok(DeviceFlowState {
            device_code: device.device_code,
            user_code: device.user_code,
            verification_url,
            expires_at,
            interval_secs: device.interval.unwrap_or(DEFAULT_WAIT_SECS),
        })
    }

    pub async fn start_loopback_flow(&self) -> AppResult<LoopbackFlowState> {
        let listener = TcpListener::bind((LOOPBACK_HOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let redirect_url = format!("http://{LOOPBACK_HOST}:{port}{LOOPBACK_PATH}");
        let state = random_token(24);
        let code_verifier = random_verifier(64);
        let code_challenge = build_code_challenge(&code_verifier)?;
        let expires_at = Utc::now() + Duration::minutes(10);

        let mut auth_url = Url::parse(&self.config.auth_endpoint).map_err(|err| {
            AppError::Config(format!("invalid Google auth endpoint: {err}"))
        })?;
        auth_url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &redirect_url)
            .append_pair("scope", &self.config.scopes)
            .append_pair("access_type", "offline")
            .append_pair("prompt", "consent")
            .append_pair("state", &state)
            .append_pair("code_challenge", &code_challenge)
            .append_pair("code_challenge_method", "S256");

        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let result = handle_loopback_callback(listener).await;
            let _ = tx.send(result);
        });

        let mut pending = self.pending_auth.lock();
        *pending = Some(LoopbackSession {
            state: state.clone(),
            code_verifier,
            redirect_url: redirect_url.clone(),
            receiver: rx,
            expires_at,
        });

        Ok(LoopbackFlowState {
            authorization_url: auth_url.to_string(),
            redirect_url,
            expires_at,
        })
    }

    pub async fn complete_loopback_flow(
        &self,
        timeout_secs: Option<u64>,
    ) -> AppResult<GoogleIdentity> {
        let (receiver, state, code_verifier, redirect_url, expires_at) = {
            let mut pending = self.pending_auth.lock();
            let session = pending
                .take()
                .ok_or_else(|| AppError::Config("start Google sign-in before approving.".into()))?;
            (
                session.receiver,
                session.state,
                session.code_verifier,
                session.redirect_url,
                session.expires_at,
            )
        };

        if Utc::now() > expires_at {
            return Err(AppError::Config(
                "google sign-in expired; restart the flow.".into(),
            ));
        }

        let wait_secs = timeout_secs.unwrap_or(DEFAULT_LOOPBACK_TIMEOUT_SECS).max(5);
        let callback_result = timeout(StdDuration::from_secs(wait_secs), receiver)
            .await
            .map_err(|_| AppError::Config("timed out waiting for Google approval".into()))?
            .map_err(|_| AppError::Config("google sign-in listener cancelled".into()))?;
        let callback = callback_result?;

        if callback.state != state {
            return Err(AppError::Config(
                "google sign-in failed state verification".into(),
            ));
        }

        let token_response = self
            .exchange_code_for_token(&callback.code, &redirect_url, &code_verifier)
            .await?;
        let token = self.store_token(token_response, None)?;
        self.fetch_identity(&token).await
    }

    pub async fn current_identity(&self) -> AppResult<GoogleIdentity> {
        let token = self.ensure_token().await?;
        self.fetch_identity(&token).await
    }

    pub fn sign_out(&self) -> AppResult<()> {
        let mut pending = self.pending_auth.lock();
        *pending = None;
        self.vault.delete(TOKEN_ALIAS)?;
        Ok(())
    }

    pub async fn complete_device_flow(
        &self,
        device_code: &str,
        interval_secs: u64,
    ) -> AppResult<GoogleIdentity> {
        let mut wait = StdDuration::from_secs(interval_secs.max(1));

        loop {
            let response = self
                .http
                .post(&self.config.token_endpoint)
                .form(&[
                    ("client_id", self.config.client_id.as_str()),
                    ("client_secret", self.config.client_secret.as_str()),
                    ("device_code", device_code),
                    ("grant_type", DEVICE_GRANT_TYPE),
                ])
                .send()
                .await?;

            if response.status().is_success() {
                let success: TokenSuccessResponse = response.json().await?;
                let token = self.store_token(success, None)?;
                return self.fetch_identity(&token).await;
            }

            let status = response.status();
            let err: TokenErrorResponse = response.json().await.unwrap_or(TokenErrorResponse {
                error: "unknown_error".into(),
                error_description: None,
            });

            match err.error.as_str() {
                "authorization_pending" => {
                    sleep(wait).await;
                }
                "slow_down" => {
                    wait += StdDuration::from_secs(5);
                    sleep(wait).await;
                }
                "expired_token" | "access_denied" => {
                    return Err(AppError::Config(format!(
                        "google sign-in failed ({status}): {}",
                        err.error
                    )));
                }
                other => {
                    return Err(AppError::Config(format!(
                        "google sign-in failed ({status}): {}",
                        other
                    )));
                }
            }
        }
    }

    pub async fn ensure_token(&self) -> AppResult<StoredGoogleToken> {
        match self.load_token()? {
            Some(token) if !token.is_expired() => {
                self.update_next_refresh(&token);
                Ok(token)
            }
            Some(token) => {
                let refresh = token.refresh_token.ok_or_else(|| {
                    AppError::Config("google token expired and cannot refresh".into())
                })?;
                let refreshed = self.refresh_token(&refresh).await?;
                self.update_next_refresh(&refreshed);
                Ok(refreshed)
            }
            None => Err(AppError::Config(
                "google sign-in required before importing Drive files".into(),
            )),
        }
    }

    pub async fn refresh_if_due(&self) -> AppResult<StoredGoogleToken> {
        let token = self.ensure_token().await?;
        let due = {
            let next = *self.refresh_state.next_refresh.lock();
            next.map(|when| Utc::now() >= when).unwrap_or(true)
        };
        if !due {
            return Ok(token);
        }

        let should_refresh = {
            let mut refreshing = self.refresh_state.refreshing.lock();
            if *refreshing {
                false
            } else {
                *refreshing = true;
                true
            }
        };
        if !should_refresh {
            return Ok(token);
        }

        let result = match token.refresh_token {
            Some(ref refresh) => self.refresh_token(refresh).await,
            None => Err(AppError::Config(
                "google token expired and cannot refresh".into(),
            )),
        };

        {
            let mut refreshing = self.refresh_state.refreshing.lock();
            *refreshing = false;
        }

        match result {
            Ok(new_token) => {
                self.update_next_refresh(&new_token);
                *self.refresh_state.last_failure.lock() = None;
                Ok(new_token)
            }
            Err(err) => {
                let retry_at = Utc::now() + Duration::minutes(1);
                *self.refresh_state.next_refresh.lock() = Some(retry_at);
                let message = err.to_string();
                *self.refresh_state.last_failure.lock() = Some(message.clone());
                let _ = self.telemetry.record(
                    "refresh_error",
                    serde_json::json!({
                        "reason": sanitize_error_copy(&message),
                        "retry_at": retry_at,
                    }),
                );
                let _ = self.persist_refresh_state(&token, Some(&message));
                Err(err)
            }
        }
    }

    pub async fn list_kml_files(&self, limit: Option<usize>) -> AppResult<Vec<DriveFileMetadata>> {
        let token = self.ensure_token().await?;
        let mut url = self.drive_url()?;
        url.path_segments_mut()
            .map_err(|_| AppError::Config("invalid Drive API base".into()))?
            .push("files");

        let page_size = limit.unwrap_or(self.config.picker_page_size).clamp(1, 100);
        url.query_pairs_mut()
            .append_pair(
                "q",
                &format!("mimeType='{}' and trashed = false", DRIVE_KML_MIME),
            )
            .append_pair("fields", "files(id,name,mimeType,modifiedTime,size)")
            .append_pair("orderBy", "modifiedTime desc")
            .append_pair("pageSize", &page_size.to_string());

        let response = self
            .http
            .get(url)
            .bearer_auth(token.access_token)
            .send()
            .await?
            .error_for_status()?;

        let files: DriveListResponse = response.json().await?;
        Ok(files
            .files
            .into_iter()
            .map(DriveFileMetadata::from)
            .collect())
    }

    pub async fn download_file<F>(&self, file_id: &str, mut progress: F) -> AppResult<Vec<u8>>
    where
        F: FnMut(u64, Option<u64>) + Send,
    {
        let token = self.ensure_token().await?;
        let mut url = self.drive_url()?;
        url.path_segments_mut()
            .map_err(|_| AppError::Config("invalid Drive API base".into()))?
            .push("files")
            .push(file_id);
        url.set_query(Some("alt=media"));

        let response = self
            .http
            .get(url)
            .bearer_auth(token.access_token)
            .send()
            .await?
            .error_for_status()?;

        let total = response.content_length();
        progress(0, total);

        let mut stream = response.bytes_stream();
        let mut downloaded = 0_u64;
        let mut buffer = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            downloaded += chunk.len() as u64;
            buffer.extend_from_slice(&chunk);
            progress(downloaded, total);
        }

        Ok(buffer)
    }

    async fn exchange_code_for_token(
        &self,
        code: &str,
        redirect_uri: &str,
        code_verifier: &str,
    ) -> AppResult<TokenSuccessResponse> {
        let response = self
            .http
            .post(&self.config.token_endpoint)
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("code", code),
                ("code_verifier", code_verifier),
                ("grant_type", "authorization_code"),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await?;

        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            Err(AppError::Config(format!(
                "failed to exchange Google auth code ({})",
                response.status()
            )))
        }
    }

    fn store_token(
        &self,
        success: TokenSuccessResponse,
        fallback_refresh: Option<String>,
    ) -> AppResult<StoredGoogleToken> {
        let refresh_token = success.refresh_token.or(fallback_refresh);
        let token = StoredGoogleToken::new(
            success.access_token,
            refresh_token,
            success.expires_in,
            success.scope,
            success.token_type,
        );
        self.persist_refresh_state(&token, None)?;
        Ok(token)
    }

    fn update_next_refresh(&self, token: &StoredGoogleToken) {
        let next = compute_next_refresh(token.expires_at);
        *self.refresh_state.next_refresh.lock() = Some(next);
        let failure = self.refresh_state.last_failure.lock().clone();
        let _ = self.persist_refresh_state(token, failure.as_deref());
    }

    fn persist_refresh_state(
        &self,
        token: &StoredGoogleToken,
        last_failure: Option<&str>,
    ) -> AppResult<()> {
        let next = self
            .refresh_state
            .next_refresh
            .lock()
            .clone()
            .unwrap_or_else(|| compute_next_refresh(token.expires_at));
        let mut persisted = token.clone();
        persisted.next_refresh = Some(next);
        persisted.last_failure = last_failure
            .map(|s| s.to_string())
            .or_else(|| self.refresh_state.last_failure.lock().clone());
        let payload = serde_json::to_string(&persisted)?;
        self.vault
            .write_secret(TOKEN_ALIAS, &SecretString::new(payload.into()))?;
        Ok(())
    }

    fn restore_refresh_state(&self) {
        if let Ok(Some(token)) = self.load_token() {
            if let Some(next) = token.next_refresh {
                *self.refresh_state.next_refresh.lock() = Some(next);
            }
            if let Some(failure) = token.last_failure.clone() {
                *self.refresh_state.last_failure.lock() = Some(failure);
            }
        }
    }

    fn load_token(&self) -> AppResult<Option<StoredGoogleToken>> {
        let secret = self.vault.read_secret(TOKEN_ALIAS)?;
        secret
            .map(|value| serde_json::from_str::<StoredGoogleToken>(value.expose_secret()))
            .transpose()
            .map_err(AppError::from)
    }

    async fn refresh_token(&self, refresh_token: &str) -> AppResult<StoredGoogleToken> {
        let response = self
            .http
            .post(&self.config.token_endpoint)
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("client_secret", self.config.client_secret.as_str()),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await?;

        if response.status().is_success() {
            let success: TokenSuccessResponse = response.json().await?;
            self.store_token(success, Some(refresh_token.to_string()))
        } else {
            Err(AppError::Config(format!(
                "failed to refresh google token ({})",
                response.status()
            )))
        }
    }

    async fn fetch_identity(&self, token: &StoredGoogleToken) -> AppResult<GoogleIdentity> {
        let response = self
            .http
            .get(&self.config.userinfo_endpoint)
            .bearer_auth(&token.access_token)
            .send()
            .await?
            .error_for_status()?;
        let profile: UserInfoResponse = response.json().await?;
        let email = profile
            .email
            .ok_or_else(|| AppError::Config("Google profile missing email".into()))?;

        Ok(GoogleIdentity {
            email,
            name: profile.name,
            picture: profile.picture,
            expires_at: token.expires_at,
        })
    }

    fn drive_url(&self) -> AppResult<Url> {
        Url::parse(&self.config.drive_api_base)
            .map_err(|err| AppError::Config(format!("invalid Drive API base URL: {err}")))
    }

    pub async fn keepalive(&self) -> AppResult<GoogleIdentity> {
        let token = self.refresh_if_due().await?;
        if let Some(reason) = self.refresh_state.last_failure.lock().clone() {
            let _ = self.telemetry.record(
                "refresh_recovered",
                serde_json::json!({
                    "reason": sanitize_error_copy(&reason),
                }),
            );
            *self.refresh_state.last_failure.lock() = None;
            let _ = self.persist_refresh_state(&token, None);
        }
        self.fetch_identity(&token).await
    }

    pub fn last_refresh_failure(&self) -> Option<String> {
        self.refresh_state.last_failure.lock().clone()
    }

    async fn run_refresh_loop(&self) {
        loop {
            sleep(StdDuration::from_secs(60)).await;
            if let Err(err) = self.refresh_if_due().await {
                warn!(?err, "background token refresh failed");
            }
        }
    }
}

fn random_token(len: usize) -> String {
    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(len)
        .map(char::from)
        .collect()
}

fn random_verifier(len: usize) -> String {
    let mut rng = thread_rng();
    let extras = ['-', '_', '.', '~'];
    (0..len)
        .map(|_| {
            if rng.gen_bool(0.2) {
                extras[rng.gen_range(0..extras.len())]
            } else {
                rng.sample(Alphanumeric) as char
            }
        })
        .collect()
}

fn build_code_challenge(verifier: &str) -> AppResult<String> {
    if verifier.is_empty() {
        return Err(AppError::Config("missing code verifier".into()));
    }
    let digest = Sha256::digest(verifier.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(digest))
}

async fn handle_loopback_callback(
    listener: TcpListener,
) -> Result<AuthCallback, AppError> {
    let (mut socket, _) = listener.accept().await?;
    let mut buffer = [0u8; 4096];
    let read = socket.read(&mut buffer).await?;
    let request = String::from_utf8_lossy(&buffer[..read]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or_else(|| AppError::Config("invalid redirect request".into()))?;

    let url = Url::parse(&format!("http://{LOOPBACK_HOST}{path}"))
        .map_err(|err| AppError::Config(format!("failed to parse redirect: {err}")))?;
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string());
    let error = url
        .query_pairs()
        .find(|(k, _)| k == "error")
        .map(|(_, v)| v.to_string());
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());

    let (status, body, result) = match (code, state, error) {
        (Some(code), Some(state), None) => {
            let body = success_body("Google sign-in approved. You can close this window.");
            ("200 OK", body, Ok(AuthCallback { code, state }))
        }
        (_, _, Some(err)) => {
            let body = error_body("Google sign-in was denied. You may close this window.");
            (
                "400 Bad Request",
                body,
                Err(AppError::Config(format!("google sign-in failed: {err}"))),
            )
        }
        _ => {
            let body = error_body("Missing authorization code. Please retry sign-in.");
            (
                "400 Bad Request",
                body,
                Err(AppError::Config("google sign-in missing code".into())),
            )
        }
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.shutdown().await;

    result
}

fn success_body(message: &str) -> String {
    format!(
        "<html><body><h3>{message}</h3><p>You can return to the app.</p></body></html>"
    )
}

fn error_body(message: &str) -> String {
    format!(
        "<html><body><h3>{message}</h3><p>Close this window and restart sign-in.</p></body></html>"
    )
}


#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    #[serde(alias = "verification_uri")]
    verification_uri: Option<String>,
    #[serde(alias = "verification_url")]
    verification_url: Option<String>,
    #[serde(alias = "verification_uri_complete")]
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

#[derive(Deserialize)]
struct TokenSuccessResponse {
    access_token: String,
    expires_in: u64,
    scope: String,
    token_type: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[allow(dead_code)]
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct DriveListResponse {
    files: Vec<DriveFileRaw>,
}

#[derive(Deserialize)]
struct DriveFileRaw {
    id: String,
    name: String,
    #[serde(rename = "mimeType")]
    mime_type: String,
    #[serde(rename = "modifiedTime")]
    modified_time: Option<String>,
    size: Option<String>,
}

#[derive(Deserialize)]
struct UserInfoResponse {
    email: Option<String>,
    name: Option<String>,
    picture: Option<String>,
}

fn compute_next_refresh(expires_at: DateTime<Utc>) -> DateTime<Utc> {
    let target = expires_at - Duration::minutes(5);
    if target > Utc::now() {
        target
    } else {
        Utc::now() + Duration::minutes(1)
    }
}
