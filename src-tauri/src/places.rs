use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use parking_lot::Mutex;
use rand::{rngs::StdRng, Rng, SeedableRng};
use reqwest::StatusCode;
use rusqlite::{Connection, OptionalExtension};
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::{sleep, Instant};
use tracing::{trace, warn};

use crate::config::AppConfig;
use crate::errors::{AppError, AppResult};
use crate::ingestion::{ListSlot, NormalizedRow, ParsedRow};

const GEO_EPSILON: f64 = 0.00001;
const MAX_ATTEMPTS: u32 = 5;
const BASE_BACKOFF_MS: u64 = 250;

fn cache_ttl_from_hours(hours: u64) -> Option<Duration> {
    if hours == 0 {
        None
    } else {
        Some(Duration::from_secs(hours.saturating_mul(3600)))
    }
}

#[derive(Debug, Clone)]
struct RawRow {
    source_hash: String,
    row: NormalizedRow,
}

#[derive(Debug, Clone, Serialize)]
pub struct NormalizationStats {
    pub slot: ListSlot,
    pub total_rows: usize,
    pub cache_hits: usize,
    pub cache_misses: usize,
    pub stale_cache: usize,
    pub places_calls: usize,
    pub resolved: usize,
    pub unresolved: usize,
    pub places_counters: PlacesCountersSnapshot,
}

impl NormalizationStats {
    fn empty(slot: ListSlot) -> Self {
        Self {
            slot,
            total_rows: 0,
            cache_hits: 0,
            cache_misses: 0,
            stale_cache: 0,
            places_calls: 0,
            resolved: 0,
            unresolved: 0,
            places_counters: PlacesCountersSnapshot::default(),
        }
    }

    fn with_total(slot: ListSlot, total_rows: usize) -> Self {
        Self {
            total_rows,
            ..Self::empty(slot)
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Default)]
pub struct PlacesCountersSnapshot {
    pub total_requests: u64,
    pub successes: u64,
    pub quota_errors: u64,
    pub invalid_key_errors: u64,
    pub network_errors: u64,
    pub other_errors: u64,
}

#[derive(Debug, Clone)]
struct NormalizationResult {
    source: ResolutionSource,
    details: PlaceDetails,
    cache_outcome: CacheOutcome,
}

#[derive(Debug, Clone)]
pub struct NormalizationProgress {
    pub slot: ListSlot,
    pub total_rows: usize,
    pub processed: usize,
    pub resolved: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlacesErrorKind {
    Quota,
    InvalidKey,
    Network,
    Other,
}

impl PlacesErrorKind {
    fn as_str(&self) -> &'static str {
        match self {
            PlacesErrorKind::Quota => "quota",
            PlacesErrorKind::InvalidKey => "invalid_key",
            PlacesErrorKind::Network => "network",
            PlacesErrorKind::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolutionSource {
    Provided,
    Cache,
    PlacesTable,
    Api,
}

#[derive(Debug, Clone)]
enum CacheOutcome {
    Fresh(String),
    Stale(String),
    Miss,
    Skipped,
}

#[derive(Debug, Clone)]
pub struct PlaceDetails {
    pub place_id: String,
    pub name: String,
    pub formatted_address: Option<String>,
    pub lat: f64,
    pub lng: f64,
    pub types: Vec<String>,
}

impl PlaceDetails {
    fn ensure_coordinates(mut self, row: &NormalizedRow) -> Self {
        if self.lat == 0.0 && self.lng == 0.0 {
            self.lat = row.latitude;
            self.lng = row.longitude;
        }
        self
    }
}

#[derive(Default)]
struct PlacesClientCounters {
    total_requests: AtomicU64,
    successes: AtomicU64,
    quota_errors: AtomicU64,
    invalid_key_errors: AtomicU64,
    network_errors: AtomicU64,
    other_errors: AtomicU64,
}

impl PlacesClientCounters {
    fn record_attempt(&self) {
        self.total_requests.fetch_add(1, Ordering::SeqCst);
    }

    fn record_success(&self) {
        self.successes.fetch_add(1, Ordering::SeqCst);
    }

    fn record_error(&self, kind: PlacesErrorKind) {
        match kind {
            PlacesErrorKind::Quota => {
                self.quota_errors.fetch_add(1, Ordering::SeqCst);
            }
            PlacesErrorKind::InvalidKey => {
                self.invalid_key_errors.fetch_add(1, Ordering::SeqCst);
            }
            PlacesErrorKind::Network => {
                self.network_errors.fetch_add(1, Ordering::SeqCst);
            }
            PlacesErrorKind::Other => {
                self.other_errors.fetch_add(1, Ordering::SeqCst);
            }
        }
    }

    fn snapshot(&self) -> PlacesCountersSnapshot {
        PlacesCountersSnapshot {
            total_requests: self.total_requests.load(Ordering::SeqCst),
            successes: self.successes.load(Ordering::SeqCst),
            quota_errors: self.quota_errors.load(Ordering::SeqCst),
            invalid_key_errors: self.invalid_key_errors.load(Ordering::SeqCst),
            network_errors: self.network_errors.load(Ordering::SeqCst),
            other_errors: self.other_errors.load(Ordering::SeqCst),
        }
    }
}

pub struct PlaceNormalizer {
    db: Arc<Mutex<Connection>>,
    lookup: PlacesService,
    rate_limiter: RateLimiter,
    jitter_rng: Arc<Mutex<StdRng>>,
    cache_ttl: Option<Duration>,
    guard: Arc<AsyncMutex<()>>,
}

impl PlaceNormalizer {
    pub fn new(db: Arc<Mutex<Connection>>, config: &AppConfig) -> Self {
        let lookup = PlacesService::new(config);
        let rate_limiter = RateLimiter::new(config.places_rate_limit_qps.max(1));
        let cache_ttl = cache_ttl_from_hours(config.normalization_cache_ttl_hours);
        Self {
            db,
            lookup,
            rate_limiter,
            jitter_rng: Arc::new(Mutex::new(StdRng::from_entropy())),
            cache_ttl,
            guard: Arc::new(AsyncMutex::new(())),
        }
    }

    #[cfg(test)]
    pub fn with_lookup(
        db: Arc<Mutex<Connection>>,
        lookup: PlacesService,
        qps: u32,
        rng: StdRng,
        cache_ttl: Duration,
    ) -> Self {
        Self {
            db,
            lookup,
            rate_limiter: RateLimiter::new(qps.max(1)),
            jitter_rng: Arc::new(Mutex::new(rng)),
            cache_ttl: Some(cache_ttl),
            guard: Arc::new(AsyncMutex::new(())),
        }
    }

    pub fn set_rate_limit(&self, qps: u32) {
        self.rate_limiter.set_qps(qps.max(1));
    }

    pub fn rate_limit_qps(&self) -> u32 {
        self.rate_limiter.qps()
    }

    pub async fn normalize_slot(
        &self,
        project_id: i64,
        slot: ListSlot,
        observer: Option<Arc<dyn Fn(NormalizationProgress) + Send + Sync>>,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> AppResult<NormalizationStats> {
        let _lock = self.guard.lock().await;
        let Some((list_id, rows)) = self.load_rows(project_id, slot)? else {
            let mut empty = NormalizationStats::empty(slot);
            empty.places_counters = self.lookup.counters_snapshot();
            return Ok(empty);
        };

        if rows.is_empty() {
            let mut empty = NormalizationStats::empty(slot);
            empty.places_counters = self.lookup.counters_snapshot();
            return Ok(empty);
        }

        self.clear_assignments(list_id)?;

        let mut stats = NormalizationStats::with_total(slot, rows.len());
        let total_rows = rows.len();
        let mut processed = 0;
        for entry in rows {
            if let Some(flag) = &cancel_flag {
                if flag.load(Ordering::SeqCst) {
                    break;
                }
            }
            match self.normalize_row(&entry).await {
                Ok(Some(result)) => {
                    match result.cache_outcome {
                        CacheOutcome::Fresh(_) => {
                            stats.cache_hits += 1;
                        }
                        CacheOutcome::Stale(_) => {
                            stats.cache_misses += 1;
                            stats.stale_cache += 1;
                        }
                        CacheOutcome::Miss => {
                            stats.cache_misses += 1;
                        }
                        CacheOutcome::Skipped => {}
                    }
                    if matches!(result.source, ResolutionSource::Api) {
                        stats.places_calls += 1;
                    }
                    self.persist_assignment(list_id, &entry, result.details)?;
                    stats.resolved += 1;
                }
                Ok(None) => {
                    stats.unresolved += 1;
                }
                Err(err) => {
                    warn!(?err, slot = ?slot, "failed to normalize row");
                    stats.unresolved += 1;
                }
            }
            processed += 1;
            if let Some(callback) = &observer {
                callback(NormalizationProgress {
                    slot,
                    total_rows,
                    processed,
                    resolved: stats.resolved,
                });
            }
        }

        if let Some(flag) = &cancel_flag {
            if flag.load(Ordering::SeqCst) && processed < total_rows {
                stats.unresolved += total_rows - processed;
            }
        }
        stats.places_counters = self.lookup.counters_snapshot();

        Ok(stats)
    }

    pub async fn refresh_slots(
        &self,
        project_id: i64,
        slots: &[ListSlot],
        observer: Option<Arc<dyn Fn(NormalizationProgress) + Send + Sync>>,
        cancel_flag: Option<Arc<AtomicBool>>,
    ) -> AppResult<Vec<NormalizationStats>> {
        let mut results = Vec::new();
        for slot in slots {
            results.push(
                self.normalize_slot(project_id, *slot, observer.clone(), cancel_flag.clone())
                    .await?,
            );
        }
        Ok(results)
    }

    fn load_rows(&self, project_id: i64, slot: ListSlot) -> AppResult<Option<(i64, Vec<RawRow>)>> {
        let (list_id, raw_rows) = {
            let conn = self.db.lock();
            let list_id: Option<i64> = conn
                .query_row(
                    "SELECT id FROM lists WHERE project_id = ?1 AND slot = ?2 LIMIT 1",
                    (project_id, slot.as_tag()),
                    |row| row.get(0),
                )
                .optional()?;
            let Some(list_id) = list_id else {
                return Ok(None);
            };

            let mut stmt = conn.prepare(
                "SELECT source_row_hash, raw_json FROM raw_items WHERE list_id = ?1 ORDER BY id ASC",
            )?;
            let rows = stmt
                .query_map([list_id], |row| {
                    let hash: String = row.get(0)?;
                    let payload: String = row.get(1)?;
                    Ok((hash, payload))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            (list_id, rows)
        };

        let mut rows = Vec::with_capacity(raw_rows.len());
        for (hash, payload) in raw_rows {
            if let Ok(parsed) = serde_json::from_str::<ParsedRow>(&payload) {
                rows.push(RawRow {
                    source_hash: if parsed.source_row_hash.is_empty() {
                        hash.clone()
                    } else {
                        parsed.source_row_hash.clone()
                    },
                    row: parsed.normalized,
                });
                continue;
            }

            let normalized: NormalizedRow = serde_json::from_str(&payload)?;
            rows.push(RawRow {
                source_hash: hash,
                row: normalized,
            });
        }
        Ok(Some((list_id, rows)))
    }

    fn clear_assignments(&self, list_id: i64) -> AppResult<()> {
        let conn = self.db.lock();
        conn.execute("DELETE FROM list_places WHERE list_id = ?1", [list_id])?;
        Ok(())
    }

    async fn normalize_row(&self, entry: &RawRow) -> AppResult<Option<NormalizationResult>> {
        if let Some(place_id) = entry.row.place_id.clone() {
            let details = self
                .load_place_by_id(&place_id)?
                .unwrap_or_else(|| details_from_row(&entry.row, place_id));
            return Ok(Some(NormalizationResult {
                source: ResolutionSource::Provided,
                details,
                cache_outcome: CacheOutcome::Skipped,
            }));
        }

        let cache_outcome = self.lookup_cache(&entry.source_hash)?;
        let cache_marker = cache_outcome.clone();
        if let CacheOutcome::Fresh(place_id) = cache_outcome {
            let details = self
                .load_place_by_id(&place_id)?
                .unwrap_or_else(|| details_from_row(&entry.row, place_id.clone()));
            return Ok(Some(NormalizationResult {
                source: ResolutionSource::Cache,
                details,
                cache_outcome: CacheOutcome::Fresh(place_id),
            }));
        }

        let allow_coordinate_cache = !matches!(cache_marker, CacheOutcome::Stale(_));
        if allow_coordinate_cache {
            if let Some(details) = self.lookup_coordinates(&entry.row)? {
                let place_id = details.place_id.clone();
                return Ok(Some(NormalizationResult {
                    source: ResolutionSource::PlacesTable,
                    details,
                    cache_outcome: CacheOutcome::Fresh(place_id),
                }));
            }
        }

        let details = self.lookup_with_retry(&entry.row).await?;
        let finalized = details.ensure_coordinates(&entry.row);
        Ok(Some(NormalizationResult {
            source: ResolutionSource::Api,
            details: finalized,
            cache_outcome: match cache_marker {
                CacheOutcome::Stale(value) => CacheOutcome::Stale(value),
                _ => CacheOutcome::Miss,
            },
        }))
    }

    fn lookup_cache(&self, source_hash: &str) -> AppResult<CacheOutcome> {
        let conn = self.db.lock();
        let record: Option<(String, String)> = conn
            .query_row(
                "SELECT place_id, created_at FROM normalization_cache WHERE source_row_hash = ?1",
                [source_hash],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let Some((place_id, created_at)) = record else {
            return Ok(CacheOutcome::Miss);
        };

        if let Some(ttl) = self.cache_ttl {
            let ttl_secs = ttl.as_secs() as f64;
            let age_secs: f64 = conn
                .query_row(
                    "SELECT (julianday('now') - julianday(?1)) * 86400.0",
                    [created_at],
                    |row| row.get(0),
                )
                .unwrap_or(ttl_secs + 1.0);
            if age_secs > ttl_secs {
                return Ok(CacheOutcome::Stale(place_id));
            }
        }

        Ok(CacheOutcome::Fresh(place_id))
    }

    fn lookup_coordinates(&self, row: &NormalizedRow) -> AppResult<Option<PlaceDetails>> {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT place_id, name, formatted_address, lat, lng, types
            FROM places
            WHERE ABS(lat - ?1) <= ?3 AND ABS(lng - ?2) <= ?3
            LIMIT 1",
            (row.latitude, row.longitude, GEO_EPSILON),
            |row| parse_place_details(row),
        )
        .optional()
        .map_err(AppError::from)
    }

    fn load_place_by_id(&self, place_id: &str) -> AppResult<Option<PlaceDetails>> {
        let conn = self.db.lock();
        conn.query_row(
            "SELECT place_id, name, formatted_address, lat, lng, types
            FROM places
            WHERE place_id = ?1",
            [place_id],
            |row| parse_place_details(row),
        )
        .optional()
        .map_err(AppError::from)
    }

    async fn lookup_with_retry(&self, row: &NormalizedRow) -> AppResult<PlaceDetails> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            self.rate_limiter.wait().await;
            match self.lookup.lookup_place(row).await {
                Ok(details) => return Ok(details),
                Err(err) if attempt < MAX_ATTEMPTS => {
                    let kind = classify_places_error(&err);
                    if matches!(kind, PlacesErrorKind::InvalidKey) {
                        return Err(err);
                    }
                    let delay = self.backoff_delay(attempt);
                    warn!(
                        ?err,
                        attempt,
                        category = kind.as_str(),
                        "places lookup failed; retrying after {:?}",
                        delay
                    );
                    sleep(delay).await;
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn backoff_delay(&self, attempt: u32) -> Duration {
        let exponent = (attempt - 1).min(6);
        let base = Duration::from_millis(BASE_BACKOFF_MS * (1 << exponent));
        let jitter = {
            let mut rng = self.jitter_rng.lock();
            let jitter_ms = rng.gen_range(0..BASE_BACKOFF_MS);
            Duration::from_millis(jitter_ms)
        };
        base + jitter
    }

    fn persist_assignment(
        &self,
        list_id: i64,
        entry: &RawRow,
        mut details: PlaceDetails,
    ) -> AppResult<()> {
        details.name = if details.name.trim().is_empty() {
            entry.row.title.clone()
        } else {
            details.name
        };
        details.formatted_address = details
            .formatted_address
            .or_else(|| entry.row.description.clone());

        {
            let conn = self.db.lock();
            conn.execute(
                "INSERT INTO places (place_id, name, formatted_address, lat, lng, types, last_checked_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, DATETIME('now'))
                ON CONFLICT(place_id) DO UPDATE SET
                    name = excluded.name,
                    formatted_address = COALESCE(excluded.formatted_address, places.formatted_address),
                    lat = excluded.lat,
                    lng = excluded.lng,
                    types = excluded.types,
                    last_checked_at = DATETIME('now')",
                (
                    details.place_id.as_str(),
                    details.name.as_str(),
                    details.formatted_address.as_deref(),
                    details.lat,
                    details.lng,
                    serialize_types(&details.types),
                ),
            )?;

            conn.execute(
                "INSERT INTO normalization_cache (source_row_hash, place_id, created_at)
                VALUES (?1, ?2, DATETIME('now'))
                ON CONFLICT(source_row_hash) DO UPDATE SET
                    place_id = excluded.place_id,
                    created_at = DATETIME('now')",
                (&entry.source_hash, details.place_id.as_str()),
            )?;

            conn.execute(
                "INSERT INTO list_places (list_id, place_id, assigned_at)
                VALUES (?1, ?2, DATETIME('now'))
                ON CONFLICT(list_id, place_id) DO UPDATE SET
                    assigned_at = excluded.assigned_at",
                (list_id, details.place_id.as_str()),
            )?;
        }

        trace!(
            list_id,
            place_id = details.place_id,
            "normalized place assignment recorded"
        );
        Ok(())
    }
}

fn details_from_row(row: &NormalizedRow, place_id: String) -> PlaceDetails {
    PlaceDetails {
        place_id,
        name: row.title.clone(),
        formatted_address: row.description.clone(),
        lat: row.latitude,
        lng: row.longitude,
        types: Vec::new(),
    }
}

fn serialize_types(types: &[String]) -> Option<String> {
    if types.is_empty() {
        None
    } else {
        Some(serde_json::to_string(types).unwrap_or_default())
    }
}

fn parse_types(value: Option<String>) -> Vec<String> {
    value
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

fn parse_place_details(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlaceDetails> {
    let place_id: String = row.get(0)?;
    let name: String = row.get(1)?;
    let formatted_address: Option<String> = row.get(2)?;
    let lat: f64 = row.get(3)?;
    let lng: f64 = row.get(4)?;
    let types: Option<String> = row.get(5)?;
    Ok(PlaceDetails {
        place_id,
        name,
        formatted_address,
        lat,
        lng,
        types: parse_types(types),
    })
}

#[derive(Clone)]
pub struct PlacesService {
    inner: Arc<dyn PlaceLookup>,
    counters: Arc<PlacesClientCounters>,
}

impl PlacesService {
    pub fn new(config: &AppConfig) -> Self {
        let counters = Arc::new(PlacesClientCounters::default());
        if let Some(key) = config.google_places_api_key.clone() {
            let http = HttpPlacesClient::new(key, Arc::clone(&counters));
            let synthetic = SyntheticPlacesClient::default();
            let client = HybridPlacesClient::new(http, synthetic);
            Self {
                inner: Arc::new(client),
                counters,
            }
        } else {
            Self {
                inner: Arc::new(SyntheticPlacesClient::default()),
                counters,
            }
        }
    }

    #[cfg(test)]
    pub fn from_lookup(lookup: Arc<dyn PlaceLookup>) -> Self {
        Self {
            inner: lookup,
            counters: Arc::new(PlacesClientCounters::default()),
        }
    }

    pub async fn lookup_place(&self, row: &NormalizedRow) -> AppResult<PlaceDetails> {
        self.inner.lookup_place(row).await
    }

    pub fn counters_snapshot(&self) -> PlacesCountersSnapshot {
        self.counters.snapshot()
    }
}

#[async_trait]
pub trait PlaceLookup: Send + Sync {
    async fn lookup_place(&self, row: &NormalizedRow) -> AppResult<PlaceDetails>;
}

struct RateLimiter {
    min_interval_ms: AtomicU64,
    last_tick: AsyncMutex<Option<Instant>>,
}

impl RateLimiter {
    fn new(qps: u32) -> Self {
        Self {
            min_interval_ms: AtomicU64::new(Self::interval_ms(qps)),
            last_tick: AsyncMutex::new(None),
        }
    }

    fn set_qps(&self, qps: u32) {
        self.min_interval_ms
            .store(Self::interval_ms(qps), Ordering::SeqCst);
    }

    fn qps(&self) -> u32 {
        let interval = self.min_interval_ms.load(Ordering::SeqCst).max(1);
        let qps = (1000_f64 / interval as f64).round() as u32;
        qps.max(1)
    }

    fn interval_ms(qps: u32) -> u64 {
        let safe_qps = qps.max(1);
        let interval_ms = (1000_f64 / safe_qps as f64).ceil() as u64;
        interval_ms.max(50)
    }

    fn interval_duration(&self) -> Duration {
        Duration::from_millis(self.min_interval_ms.load(Ordering::SeqCst))
    }

    async fn wait(&self) {
        let interval = self.interval_duration();
        let mut guard = self.last_tick.lock().await;
        if let Some(prev) = *guard {
            let elapsed = prev.elapsed();
            if elapsed < interval {
                sleep(interval - elapsed).await;
            }
        }
        *guard = Some(Instant::now());
    }
}

fn classify_places_error(err: &AppError) -> PlacesErrorKind {
    match err {
        AppError::Http(http_err) => {
            if http_err.is_timeout() || http_err.is_connect() {
                return PlacesErrorKind::Network;
            }
            if let Some(status) = http_err.status() {
                if status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::SERVICE_UNAVAILABLE
                {
                    return PlacesErrorKind::Quota;
                }
                if status == StatusCode::UNAUTHORIZED
                    || status == StatusCode::FORBIDDEN
                    || status == StatusCode::PAYMENT_REQUIRED
                {
                    return PlacesErrorKind::InvalidKey;
                }
            }
            PlacesErrorKind::Other
        }
        _ => PlacesErrorKind::Other,
    }
}

struct HybridPlacesClient {
    primary: HttpPlacesClient,
    fallback: SyntheticPlacesClient,
}

impl HybridPlacesClient {
    fn new(primary: HttpPlacesClient, fallback: SyntheticPlacesClient) -> Self {
        Self { primary, fallback }
    }
}

#[async_trait]
impl PlaceLookup for HybridPlacesClient {
    async fn lookup_place(&self, row: &NormalizedRow) -> AppResult<PlaceDetails> {
        match self.primary.lookup_place(row).await {
            Ok(details) => Ok(details),
            Err(err) => {
                warn!(
                    ?err,
                    "places http lookup failed; falling back to synthetic resolver"
                );
                self.fallback.lookup_place(row).await
            }
        }
    }
}

struct HttpPlacesClient {
    http: reqwest::Client,
    api_key: SecretString,
    counters: Arc<PlacesClientCounters>,
}

impl HttpPlacesClient {
    fn new(api_key: SecretString, counters: Arc<PlacesClientCounters>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("places http client");
        Self {
            http,
            api_key,
            counters,
        }
    }

    fn record_http_error(&self, err: reqwest::Error) -> AppError {
        let app_err: AppError = err.into();
        let kind = classify_places_error(&app_err);
        self.counters.record_error(kind);
        app_err
    }
}

#[async_trait]
impl PlaceLookup for HttpPlacesClient {
    async fn lookup_place(&self, row: &NormalizedRow) -> AppResult<PlaceDetails> {
        #[derive(serde::Serialize)]
        struct RequestBody<'a> {
            #[serde(rename = "textQuery")]
            text_query: &'a str,
            #[serde(rename = "maxResultCount")]
            max_result_count: u8,
            #[serde(rename = "locationBias")]
            location_bias: LocationBias<'a>,
        }

        #[derive(serde::Serialize)]
        struct LocationBias<'a> {
            circle: BiasCircle<'a>,
        }

        #[derive(serde::Serialize)]
        struct BiasCircle<'a> {
            center: BiasCenter<'a>,
            radius: u32,
        }

        #[derive(serde::Serialize)]
        struct BiasCenter<'a> {
            latitude: &'a f64,
            longitude: &'a f64,
        }

        #[derive(serde::Deserialize)]
        struct Response {
            places: Option<Vec<ResponsePlace>>,
        }

        #[derive(serde::Deserialize)]
        struct ResponsePlace {
            #[serde(rename = "placeId")]
            place_id: Option<String>,
            #[serde(rename = "id")]
            legacy_id: Option<String>,
            #[serde(rename = "displayName")]
            display_name: Option<ResponseText>,
            #[serde(rename = "formattedAddress")]
            formatted_address: Option<String>,
            location: Option<ResponseLocation>,
            types: Option<Vec<String>>,
        }

        #[derive(serde::Deserialize)]
        struct ResponseText {
            text: Option<String>,
        }

        #[derive(serde::Deserialize)]
        struct ResponseLocation {
            latitude: Option<f64>,
            longitude: Option<f64>,
        }

        let body = RequestBody {
            text_query: &row.title,
            max_result_count: 1,
            location_bias: LocationBias {
                circle: BiasCircle {
                    center: BiasCenter {
                        latitude: &row.latitude,
                        longitude: &row.longitude,
                    },
                    radius: 500,
                },
            },
        };

        self.counters.record_attempt();
        let response = self
            .http
            .post("https://places.googleapis.com/v1/places:searchText")
            .header("X-Goog-Api-Key", self.api_key.expose_secret())
            .header(
                "X-Goog-FieldMask",
                "places.id,places.placeId,places.displayName,places.formattedAddress,places.location,places.types",
            )
            .json(&body)
            .send()
            .await
            .map_err(|err| self.record_http_error(err))?
            .error_for_status()
            .map_err(|err| self.record_http_error(err))?;

        let parsed: Response = response.json().await.map_err(|err| {
            self.counters.record_error(PlacesErrorKind::Other);
            AppError::from(err)
        })?;
        self.counters.record_success();
        let place = parsed
            .places
            .and_then(|mut list| list.pop())
            .ok_or_else(|| AppError::Config("Places API returned no candidates".into()))?;

        let place_id = place
            .place_id
            .or(place.legacy_id)
            .ok_or_else(|| AppError::Config("Places API response missing place_id".into()))?;

        let mut lat = row.latitude;
        let mut lng = row.longitude;
        if let Some(loc) = place.location {
            if let Some(value) = loc.latitude {
                lat = value;
            }
            if let Some(value) = loc.longitude {
                lng = value;
            }
        }

        Ok(PlaceDetails {
            place_id,
            name: place
                .display_name
                .and_then(|text| text.text)
                .unwrap_or_else(|| row.title.clone()),
            formatted_address: place.formatted_address.or_else(|| row.description.clone()),
            lat,
            lng,
            types: place.types.unwrap_or_default(),
        })
    }
}

#[derive(Default)]
struct SyntheticPlacesClient;

#[async_trait]
impl PlaceLookup for SyntheticPlacesClient {
    async fn lookup_place(&self, row: &NormalizedRow) -> AppResult<PlaceDetails> {
        let mut hasher = Sha256::new();
        hasher.update(row.title.as_bytes());
        hasher.update(row.latitude.to_le_bytes());
        hasher.update(row.longitude.to_le_bytes());
        let id = base64::engine::general_purpose::STANDARD_NO_PAD.encode(hasher.finalize());
        Ok(PlaceDetails {
            place_id: format!("synthetic_{id}"),
            name: row.title.clone(),
            formatted_address: row.description.clone(),
            lat: row.latitude,
            lng: row.longitude,
            types: vec!["synthetic".into()],
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rand::SeedableRng;

    use crate::db::bootstrap;
    use crate::ingestion::{ListSlot, NormalizedRow};
    use crate::secrets::SecretVault;

    use super::*;

    struct TestPlacesClient {
        responses: Arc<Mutex<Vec<Result<PlaceDetails, AppError>>>>,
    }

    impl TestPlacesClient {
        fn new(responses: Vec<Result<PlaceDetails, AppError>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
            }
        }
    }

    #[async_trait]
    impl PlaceLookup for TestPlacesClient {
        async fn lookup_place(&self, row: &NormalizedRow) -> AppResult<PlaceDetails> {
            let mut store = self.responses.lock();
            store
                .pop()
                .unwrap_or_else(|| {
                    Ok(PlaceDetails {
                        place_id: format!("fallback_{}", row.title),
                        name: row.title.clone(),
                        formatted_address: row.description.clone(),
                        lat: row.latitude,
                        lng: row.longitude,
                        types: Vec::new(),
                    })
                })
                .map_err(|err| err)
        }
    }

    #[tokio::test]
    async fn uses_cache_before_api_call() {
        let dir = tempfile::tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let bootstrap = bootstrap(dir.path(), "places.db", &vault).unwrap();
        let db = Arc::new(Mutex::new(bootstrap.context.connection));

        let project_id: i64 = {
            let conn = db.lock();
            let project_id = conn
                .query_row(
                    "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO lists (project_id, slot, name, source) VALUES (?1, 'A', 'List A', 'test')",
                [project_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO raw_items (list_id, source_row_hash, raw_json) VALUES (1, 'hash', ?1)",
                [serde_json::to_string(&NormalizedRow {
                    title: "Cached".into(),
                    description: None,
                    longitude: 1.0,
                    latitude: 2.0,
                    altitude: None,
                    place_id: None,
                    raw_coordinates: "1,2,0".into(),
                    layer_path: None,
                })
                .unwrap()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO normalization_cache (source_row_hash, place_id) VALUES ('hash', 'cached_place')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO places (place_id, name, formatted_address, lat, lng, types, last_checked_at)
                 VALUES ('cached_place', 'Existing', NULL, 2.0, 1.0, NULL, DATETIME('now'))",
                [],
            )
            .unwrap();
            project_id
        };

        let lookup = PlacesService::from_lookup(Arc::new(TestPlacesClient::new(vec![])));
        let normalizer = PlaceNormalizer::with_lookup(
            db.clone(),
            lookup,
            3,
            rand::rngs::StdRng::seed_from_u64(1),
            Duration::from_secs(3600),
        );

        let stats = normalizer
            .normalize_slot(project_id, ListSlot::A, None, None)
            .await
            .unwrap();
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.cache_misses, 0);
        assert_eq!(stats.stale_cache, 0);
        assert_eq!(stats.places_calls, 0);
        assert_eq!(stats.resolved, 1);
        assert_eq!(stats.places_counters.total_requests, 0);
    }

    #[tokio::test]
    async fn stale_cache_entries_trigger_refresh() {
        let dir = tempfile::tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let bootstrap = bootstrap(dir.path(), "stale_cache.db", &vault).unwrap();
        let db = Arc::new(Mutex::new(bootstrap.context.connection));

        let project_id: i64 = {
            let conn = db.lock();
            let project_id = conn
                .query_row(
                    "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO lists (project_id, slot, name, source) VALUES (?1, 'A', 'List A', 'test')",
                [project_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO raw_items (list_id, source_row_hash, raw_json) VALUES (1, 'hash', ?1)",
                [serde_json::to_string(&NormalizedRow {
                    title: "Stale".into(),
                    description: None,
                    longitude: 1.0,
                    latitude: 2.0,
                    altitude: None,
                    place_id: None,
                    raw_coordinates: "1,2,0".into(),
                    layer_path: None,
                })
                .unwrap()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO normalization_cache (source_row_hash, place_id, created_at) VALUES ('hash', 'stale_place', DATETIME('now', '-2 hours'))",
                [],
            )
            .unwrap();
            project_id
        };

        let lookup =
            PlacesService::from_lookup(Arc::new(TestPlacesClient::new(vec![Ok(PlaceDetails {
                place_id: "fresh_place".into(),
                name: "Refreshed".into(),
                formatted_address: None,
                lat: 2.0,
                lng: 1.0,
                types: Vec::new(),
            })])));

        let normalizer = PlaceNormalizer::with_lookup(
            db.clone(),
            lookup,
            3,
            rand::rngs::StdRng::seed_from_u64(5),
            Duration::from_secs(3600),
        );

        let stats = normalizer
            .normalize_slot(project_id, ListSlot::A, None, None)
            .await
            .unwrap();
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 1);
        assert_eq!(stats.stale_cache, 1);
        assert_eq!(stats.places_calls, 1);

        let refreshed: String = {
            let conn = db.lock();
            conn.query_row(
                "SELECT place_id FROM normalization_cache WHERE source_row_hash = 'hash'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(refreshed, "fresh_place");

        let (checked_at, assignments): (String, i64) = {
            let conn = db.lock();
            let checked: String = conn
                .query_row(
                    "SELECT last_checked_at FROM places WHERE place_id = 'fresh_place'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            let linked: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM list_places WHERE place_id = 'fresh_place'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            (checked, linked)
        };
        assert!(!checked_at.is_empty());
        assert_eq!(assignments, 1);
    }

    #[tokio::test]
    async fn retries_before_succeeding() {
        let dir = tempfile::tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let bootstrap = bootstrap(dir.path(), "retry.db", &vault).unwrap();
        let db = Arc::new(Mutex::new(bootstrap.context.connection));

        let project_id: i64 = {
            let conn = db.lock();
            let project_id = conn
                .query_row(
                    "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO lists (project_id, slot, name, source) VALUES (?1, 'A', 'List A', 'test')",
                [project_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO raw_items (list_id, source_row_hash, raw_json) VALUES (1, 'hash', ?1)",
                [serde_json::to_string(&NormalizedRow {
                    title: "Needs API".into(),
                    description: None,
                    longitude: 1.0,
                    latitude: 2.0,
                    altitude: None,
                    place_id: None,
                    raw_coordinates: "1,2,0".into(),
                    layer_path: None,
                })
                .unwrap()],
            )
            .unwrap();
            project_id
        };

        let lookup = PlacesService::from_lookup(Arc::new(TestPlacesClient::new(vec![
            Ok(PlaceDetails {
                place_id: "success".into(),
                name: "Resolved".into(),
                formatted_address: None,
                lat: 2.0,
                lng: 1.0,
                types: Vec::new(),
            }),
            Err(AppError::Config("transient".into())),
        ])));

        let normalizer = PlaceNormalizer::with_lookup(
            db.clone(),
            lookup,
            3,
            rand::rngs::StdRng::seed_from_u64(2),
            Duration::from_secs(3600),
        );

        let stats = normalizer
            .normalize_slot(project_id, ListSlot::A, None, None)
            .await
            .unwrap();
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 1);
        assert_eq!(stats.places_calls, 1);
        assert_eq!(stats.resolved, 1);
    }
}
