use httptest::matchers::{all_of, request};
use httptest::responders::{json_encoded, status_code};
use httptest::{Expectation, Server};
use serde_json::json;
use tempfile::tempdir;

use tauri_app_lib::{
    bootstrap, enqueue_place_hashes, parse_kml, persist_rows, AppConfig, DriveFileMetadata,
    GoogleServices, ListSlot, SecretVault, TelemetryClient,
};

const SAMPLE_KML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<kml xmlns="http://www.opengis.net/kml/2.2">
  <Document>
    <Placemark>
      <name>Test Spot</name>
      <Point>
        <coordinates>-122.084000,37.421998,0</coordinates>
      </Point>
      <ExtendedData>
        <Data name="PlaceID">
          <value>ChIJ123abc</value>
        </Data>
      </ExtendedData>
    </Placemark>
  </Document>
</kml>
"#;

#[tokio::test]
async fn device_flow_and_import_roundtrip() {
    let server = Server::run();
    let sample_md5 = format!("{:x}", md5::compute(SAMPLE_KML.as_bytes()));

    server.expect(
        Expectation::matching(all_of!(
            request::method("POST"),
            request::path("/device/code")
        ))
        .respond_with(json_encoded(json!({
            "device_code": "device-code",
            "user_code": "USER-CODE",
            "verification_url": "https://example.com",
            "expires_in": 1800,
            "interval": 5
        }))),
    );

    server.expect(
        Expectation::matching(all_of!(request::method("POST"), request::path("/token")))
            .respond_with(json_encoded(json!({
                "access_token": "ya29.access",
                "refresh_token": "ya29.refresh",
                "expires_in": 3600,
                "scope": "drive.readonly",
                "token_type": "Bearer"
            }))),
    );

    server.expect(
        Expectation::matching(all_of!(request::method("GET"), request::path("/userinfo")))
            .respond_with(json_encoded(json!({
                "email": "importer@example.com",
                "name": "Drive Importer",
                "picture": null
            }))),
    );

    server.expect(
        Expectation::matching(all_of!(
            request::method("GET"),
            request::path("/drive/v3/files")
        ))
        .respond_with(json_encoded(json!({
            "files": [{
                "id": "drive-file",
                "name": "List A",
                "mimeType": "application/vnd.google-earth.kml+xml",
                "modifiedTime": "2024-01-01T00:00:00Z",
                "size": SAMPLE_KML.len().to_string(),
                "md5Checksum": sample_md5
            }]
        }))),
    );

    server.expect(
        Expectation::matching(all_of!(
            request::method("GET"),
            request::path("/drive/v3/files/drive-file")
        ))
        .respond_with(
            status_code(200)
                .append_header("content-type", "application/vnd.google-earth.kml+xml")
                .body(SAMPLE_KML),
        ),
    );

    std::env::set_var("GOOGLE_OAUTH_CLIENT_ID", "test-client");
    std::env::set_var("GOOGLE_OAUTH_CLIENT_SECRET", "test-secret");
    std::env::set_var(
        "GOOGLE_DEVICE_CODE_ENDPOINT",
        server.url("/device/code").to_string(),
    );
    std::env::set_var("GOOGLE_TOKEN_ENDPOINT", server.url("/token").to_string());
    std::env::set_var(
        "GOOGLE_USERINFO_ENDPOINT",
        server.url("/userinfo").to_string(),
    );
    std::env::set_var("GOOGLE_DRIVE_API_BASE", server.url("/drive/v3").to_string());

    let vault = SecretVault::in_memory();
    let config = AppConfig::from_env();
    let temp_dir = tempdir().unwrap();
    let telemetry = TelemetryClient::new(temp_dir.path(), &config).unwrap();
    let google = GoogleServices::maybe_new(&config, &vault, telemetry)
        .expect("service creation")
        .expect("oauth configured");

    let device_flow = google.start_device_flow().await.expect("device flow");
    assert_eq!(device_flow.user_code, "USER-CODE");

    let identity = google
        .complete_device_flow(&device_flow.device_code, device_flow.interval_secs)
        .await
        .expect("sign in");
    assert_eq!(identity.email, "importer@example.com");

    let files = google.list_kml_files(Some(5)).await.expect("list files");
    assert_eq!(files.len(), 1);

    let mut checkpoints = Vec::new();
    let download = google
        .download_file(
            "drive-file",
            None,
            Some(SAMPLE_KML.len() as u64),
            Some(sample_md5.as_str()),
            |received, total| {
                checkpoints.push((received, total));
            },
        )
        .await
        .expect("download");
    assert!(!checkpoints.is_empty());
    let text = String::from_utf8(download.bytes.clone()).expect("utf8 kml");
    assert!(text.contains("<kml"));

    let parsed = parse_kml(&download.bytes).expect("parse rows");
    assert_eq!(parsed.rows.len(), 1);

    let dir = tempdir().unwrap();
    let bootstrap_ctx = bootstrap(dir.path(), "import.db", &vault).expect("bootstrap db");
    let mut connection = bootstrap_ctx.context.connection;
    let project_id: i64 = connection
        .query_row(
            "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .expect("project id");
    let drive_file = DriveFileMetadata {
        id: "drive-file".into(),
        name: "List A".into(),
        mime_type: "application/vnd.google-earth.kml+xml".into(),
        modified_time: Some("2024-01-01T00:00:00Z".into()),
        size: Some(SAMPLE_KML.len() as u64),
        md5_checksum: Some(sample_md5),
    };
    let summary = persist_rows(
        &mut connection,
        project_id,
        ListSlot::A,
        &drive_file,
        &parsed.rows,
    )
    .expect("persist rows");
    assert_eq!(summary.row_count, 1);

    let telemetry = TelemetryClient::new(dir.path(), &config).expect("telemetry");
    enqueue_place_hashes(&telemetry, ListSlot::A, &parsed.rows).expect("hash telemetry");
}
