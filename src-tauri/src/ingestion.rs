use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use roxmltree::{Document, Node};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::{AppError, AppResult};
use crate::google::DriveFileMetadata;
use crate::telemetry::TelemetryClient;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ListSlot {
    A,
    B,
}

impl ListSlot {
    pub fn as_tag(&self) -> &'static str {
        match self {
            ListSlot::A => "A",
            ListSlot::B => "B",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            ListSlot::A => "List A",
            ListSlot::B => "List B",
        }
    }

    pub fn parse(value: &str) -> AppResult<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "A" => Ok(ListSlot::A),
            "B" => Ok(ListSlot::B),
            _ => Err(AppError::Config(format!("invalid list slot: {value}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedRow {
    pub title: String,
    pub description: Option<String>,
    pub longitude: f64,
    pub latitude: f64,
    pub altitude: Option<f64>,
    pub place_id: Option<String>,
    pub raw_coordinates: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_path: Option<String>,
}

impl NormalizedRow {
    pub fn source_hash(&self) -> String {
        let mut hasher = Sha256::new();
        let serialized =
            serde_json::to_string(self).expect("normalized rows serialize deterministically");
        hasher.update(serialized.as_bytes());
        STANDARD_NO_PAD.encode(hasher.finalize())
    }

    pub fn place_hash(&self) -> String {
        let mut hasher = Sha256::new();
        if let Some(place_id) = &self.place_id {
            hasher.update(place_id.as_bytes());
        } else {
            hasher.update(self.title.as_bytes());
            hasher.update(self.latitude.to_le_bytes());
            hasher.update(self.longitude.to_le_bytes());
        }
        STANDARD_NO_PAD.encode(hasher.finalize())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawPlacemark {
    pub name: Option<String>,
    pub description: Option<String>,
    pub coordinates: Option<String>,
    pub place_id: Option<String>,
    pub altitude: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedRow {
    pub normalized: NormalizedRow,
    pub original: RawPlacemark,
    pub source_row_hash: String,
}

impl ParsedRow {
    fn new(normalized: NormalizedRow, original: RawPlacemark) -> Self {
        let source_row_hash = normalized.source_hash();
        Self {
            normalized,
            original,
            source_row_hash,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RejectedPlacemark {
    pub message: String,
    pub raw: RawPlacemark,
}

#[derive(Debug, Clone)]
pub struct ParsedKml {
    pub rows: Vec<ParsedRow>,
    pub rejected: Vec<RejectedPlacemark>,
}

impl ParsedKml {
    fn new(rows: Vec<ParsedRow>, rejected: Vec<RejectedPlacemark>) -> Self {
        Self { rows, rejected }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    pub list_name: String,
    pub list_id: i64,
    pub row_count: usize,
}

fn ensure_list_record(connection: &Connection, project_id: i64, slot: ListSlot) -> AppResult<i64> {
    connection.execute(
        "INSERT INTO lists (project_id, slot, name, source)
        SELECT ?1, ?2, ?3, 'drive_kml'
        WHERE NOT EXISTS (SELECT 1 FROM lists WHERE project_id = ?1 AND slot = ?2)",
        (project_id, slot.as_tag(), slot.display_name()),
    )?;

    connection
        .query_row(
            "SELECT id FROM lists WHERE project_id = ?1 AND slot = ?2 LIMIT 1",
            (project_id, slot.as_tag()),
            |row| row.get(0),
        )
        .map_err(AppError::from)
}

pub fn persist_drive_selection(
    connection: &Connection,
    project_id: i64,
    slot: ListSlot,
    drive_file: Option<&DriveFileMetadata>,
) -> AppResult<i64> {
    let list_id = ensure_list_record(connection, project_id, slot)?;
    match drive_file {
        Some(file) => {
            connection.execute(
                "UPDATE lists
                SET drive_file_id = ?1,
                    drive_file_name = ?2,
                    drive_file_mime = ?3,
                    drive_file_size = ?4,
                    drive_modified_time = ?5,
                    drive_file_checksum = ?6,
                    name = ?7
                WHERE id = ?8",
                (
                    file.id.as_str(),
                    file.name.as_str(),
                    file.mime_type.as_str(),
                    file.size.map(|value| value as i64),
                    file.modified_time.clone(),
                    file.md5_checksum.clone(),
                    slot.display_name(),
                    list_id,
                ),
            )?;
        }
        None => {
            connection.execute(
                "UPDATE lists
                SET drive_file_id = NULL,
                    drive_file_name = NULL,
                    drive_file_mime = NULL,
                    drive_file_size = NULL,
                    drive_modified_time = NULL,
                    drive_file_checksum = NULL
                WHERE id = ?1",
                [list_id],
            )?;
        }
    }
    Ok(list_id)
}

pub fn parse_kml(bytes: &[u8]) -> AppResult<ParsedKml> {
    let xml = std::str::from_utf8(bytes)
        .map_err(|err| AppError::Parse(format!("invalid UTF-8 in KML: {err}")))?;
    let document =
        Document::parse(xml).map_err(|err| AppError::Parse(format!("invalid KML: {err}")))?;

    let mut rows = Vec::new();
    let mut rejected = Vec::new();
    for placemark in document
        .descendants()
        .filter(|node| node.tag_name().name() == "Placemark")
    {
        let raw = extract_raw_placemark(placemark);
        let coordinates = match raw.coordinates.clone() {
            Some(value) => value,
            None => {
                rejected.push(RejectedPlacemark {
                    message: "Placemark missing coordinates".into(),
                    raw,
                });
                continue;
            }
        };

        let mut raw_entry = raw;
        match parse_coordinates(&coordinates) {
            Some((longitude, latitude, altitude)) => {
                let normalized = NormalizedRow {
                    title: normalize_label(raw_entry.name.as_deref())
                        .unwrap_or_else(|| "Untitled placemark".to_string()),
                    description: normalize_text(raw_entry.description.as_deref()),
                    longitude: normalize_coordinate(longitude),
                    latitude: normalize_coordinate(latitude),
                    altitude,
                    place_id: raw_entry.place_id.clone(),
                    raw_coordinates: coordinates,
                    layer_path: raw_entry.layer_path.clone(),
                };
                raw_entry.altitude = altitude;
                rows.push(ParsedRow::new(normalized, raw_entry));
            }
            None => {
                rejected.push(RejectedPlacemark {
                    message: "Placemark missing valid coordinates".into(),
                    raw: raw_entry,
                });
                continue;
            }
        }
    }

    Ok(ParsedKml::new(rows, rejected))
}

pub fn persist_rows(
    connection: &mut Connection,
    project_id: i64,
    slot: ListSlot,
    drive_file: &DriveFileMetadata,
    rows: &[ParsedRow],
) -> AppResult<ImportSummary> {
    persist_rows_with_progress(
        connection,
        project_id,
        slot,
        drive_file,
        rows,
        Option::<fn(usize, usize)>::None,
    )
}

pub fn persist_rows_with_progress<F>(
    connection: &mut Connection,
    project_id: i64,
    slot: ListSlot,
    drive_file: &DriveFileMetadata,
    rows: &[ParsedRow],
    mut progress: Option<F>,
) -> AppResult<ImportSummary>
where
    F: FnMut(usize, usize),
{
    let tx = connection.transaction()?;
    let list_name = slot.display_name();
    let list_id = persist_drive_selection(&tx, project_id, slot, Some(drive_file))?;
    tx.execute(
        "UPDATE lists SET imported_at = DATETIME('now') WHERE id = ?1",
        [list_id],
    )?;

    tx.execute("DELETE FROM raw_items WHERE list_id = ?1", [list_id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO raw_items (list_id, source_row_hash, raw_json) VALUES (?1, ?2, ?3)",
        )?;
        for (index, row) in rows.iter().enumerate() {
            stmt.execute(params![
                list_id,
                row.source_row_hash,
                serde_json::to_string(row)?
            ])?;
            if let Some(cb) = progress.as_mut() {
                cb(index + 1, rows.len());
            }
        }
    }
    tx.commit()?;

    Ok(ImportSummary {
        list_name: list_name.to_string(),
        list_id,
        row_count: rows.len(),
    })
}

pub fn enqueue_place_hashes(
    telemetry: &TelemetryClient,
    slot: ListSlot,
    rows: &[ParsedRow],
) -> AppResult<()> {
    for row in rows {
        telemetry.record_lossy(
            "raw_row_hashed",
            serde_json::json!({
                "slot": slot.as_tag(),
                "place_hash": row.normalized.place_hash(),
                "source_row_hash": row.source_row_hash,
            }),
        );
    }
    Ok(())
}

fn extract_raw_placemark(node: Node<'_, '_>) -> RawPlacemark {
    RawPlacemark {
        name: extract_tag_text(node, "name"),
        description: extract_tag_text(node, "description"),
        coordinates: extract_coordinates(node),
        place_id: extract_place_id(node),
        altitude: None,
        layer_path: resolve_layer_path(node),
    }
}

fn extract_tag_text(node: Node<'_, '_>, tag: &str) -> Option<String> {
    node.children()
        .find(|child| child.tag_name().name() == tag)
        .and_then(|child| child.text())
        .map(|value| collapse_whitespace(value))
        .filter(|value| !value.is_empty())
}

fn extract_coordinates(node: Node<'_, '_>) -> Option<String> {
    node.descendants()
        .find(|child| child.tag_name().name() == "coordinates")
        .and_then(|child| child.text())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_coordinates(value: &str) -> Option<(f64, f64, Option<f64>)> {
    let entry = value.split_whitespace().next()?;
    let mut parts = entry.split(',');
    let longitude = parts.next()?.trim().parse().ok()?;
    let latitude = parts.next()?.trim().parse().ok()?;
    let altitude = parts.next().and_then(|v| v.trim().parse().ok());
    Some((longitude, latitude, altitude))
}

fn resolve_layer_path(node: Node<'_, '_>) -> Option<String> {
    let mut path = Vec::new();
    for ancestor in node.ancestors() {
        if matches!(ancestor.tag_name().name(), "Folder" | "Document") {
            if let Some(name) = extract_tag_text(ancestor, "name") {
                path.push(name);
            }
        }
    }
    if path.is_empty() {
        None
    } else {
        path.reverse();
        Some(path.join(" / "))
    }
}

fn normalize_label(value: Option<&str>) -> Option<String> {
    value.and_then(|v| {
        let cleaned = collapse_whitespace(v);
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        }
    })
}

fn normalize_text(value: Option<&str>) -> Option<String> {
    value
        .map(|v| collapse_whitespace(v))
        .filter(|v| !v.is_empty())
}

fn normalize_coordinate(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

fn collapse_whitespace(value: &str) -> String {
    value
        .split_whitespace()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn extract_place_id(node: Node<'_, '_>) -> Option<String> {
    for candidate in node.descendants() {
        match candidate.tag_name().name() {
            "Data" | "SimpleData" => {
                if let Some(name) = candidate.attribute("name") {
                    if matches!(
                        name,
                        "PlaceID" | "placeId" | "gx_id" | "google_maps_place_id"
                    ) {
                        if let Some(value) = candidate
                            .descendants()
                            .find(|child| child.tag_name().name() == "value")
                            .and_then(|child| child.text())
                            .or_else(|| candidate.text())
                        {
                            let trimmed = value.trim();
                            if !trimmed.is_empty() {
                                return Some(trimmed.to_string());
                            }
                        }
                    }
                }
            }
            _ => continue,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::db::bootstrap;
    use crate::google::DriveFileMetadata;
    use crate::secrets::SecretVault;

    const SAMPLE_KML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
    <kml xmlns="http://www.opengis.net/kml/2.2">
      <Document>
        <Placemark id="place-1">
          <name>Example Place</name>
          <description>A nice spot</description>
          <Point>
            <coordinates>-122.084000,37.421998,0</coordinates>
          </Point>
          <ExtendedData>
            <Data name="PlaceID">
              <value>ChIJ2eUgeAK6j4ARbn5u_wAGqWA</value>
            </Data>
          </ExtendedData>
        </Placemark>
        <Placemark>
          <name>Fallback</name>
          <Point>
            <coordinates>-0.1,51.5</coordinates>
          </Point>
        </Placemark>
      </Document>
    </kml>
    "#;

    #[test]
    fn parses_kml_rows() {
        let parsed = parse_kml(SAMPLE_KML.as_bytes()).unwrap();
        assert_eq!(parsed.rows.len(), 2);
        assert_eq!(parsed.rejected.len(), 0);
        let first = &parsed.rows[0].normalized;
        assert_eq!(first.title, "Example Place");
        assert!(first.description.as_ref().unwrap().contains("nice"));
        assert!(first.place_id.is_some());
        assert!(!first.source_hash().is_empty());
        assert!(!first.place_hash().is_empty());
    }

    #[test]
    fn persists_rows_and_tracks_ids() {
        let dir = tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let bootstrap = bootstrap(dir.path(), "drive.db", &vault).unwrap();
        let mut conn = bootstrap.context.connection;
        let telemetry = TelemetryClient::new(dir.path(), &crate::config::AppConfig::from_env())
            .expect("telemetry");
        let parsed = parse_kml(SAMPLE_KML.as_bytes()).unwrap();
        let project_id: i64 = conn
            .query_row(
                "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let drive_file = DriveFileMetadata {
            id: "drive-file".into(),
            name: "List A".into(),
            mime_type: "application/vnd.google-earth.kml+xml".into(),
            modified_time: None,
            size: None,
            md5_checksum: None,
        };
        let summary = persist_rows(
            &mut conn,
            project_id,
            ListSlot::A,
            &drive_file,
            &parsed.rows,
        )
        .unwrap();
        assert_eq!(summary.row_count, 2);
        enqueue_place_hashes(&telemetry, ListSlot::A, &parsed.rows).unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM raw_items WHERE list_id = ?1",
                [summary.list_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }
}
