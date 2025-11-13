use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt;
use reqwest::{Client, Url};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

use crate::config::AppConfig;
use crate::errors::{AppError, AppResult};
use crate::secrets::SecretVault;

const TOKEN_ALIAS: &str = "google-oauth-token";
const DRIVE_KML_MIME: &str = "application/vnd.google-earth.kml+xml";
const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
const DEFAULT_WAIT_SECS: u64 = 5;

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
}

#[derive(Clone)]
struct GoogleSettings {
    client_id: String,
    client_secret: String,
    device_code_endpoint: String,
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
        }
    }

    fn is_expired(&self) -> bool {
        let buffer = Duration::seconds(15);
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

impl GoogleServices {
    pub fn maybe_new(config: &AppConfig, vault: &SecretVault) -> AppResult<Option<Self>> {
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

        Ok(Some(Self {
            http,
            config: GoogleSettings {
                client_id,
                client_secret,
                device_code_endpoint: config.google_device_code_endpoint.clone(),
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
        }))
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
            Some(token) if !token.is_expired() => Ok(token),
            Some(token) => {
                let refresh = token.refresh_token.ok_or_else(|| {
                    AppError::Config("google token expired and cannot refresh".into())
                })?;
                self.refresh_token(&refresh).await
            }
            None => Err(AppError::Config(
                "google sign-in required before importing Drive files".into(),
            )),
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
        let payload = serde_json::to_string(&token)?;
        self.vault
            .write_secret(TOKEN_ALIAS, &SecretString::new(payload.into()))?;
        Ok(token)
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
