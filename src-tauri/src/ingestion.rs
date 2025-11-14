use base64::engine::general_purpose::STANDARD_NO_PAD;
use base64::Engine;
use roxmltree::{Document, Node};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::errors::{AppError, AppResult};
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

#[derive(Debug, Clone, Serialize)]
pub struct ImportSummary {
    pub list_name: String,
    pub list_id: i64,
    pub row_count: usize,
}

pub fn parse_kml(bytes: &[u8]) -> AppResult<Vec<NormalizedRow>> {
    let xml = std::str::from_utf8(bytes)
        .map_err(|err| AppError::Parse(format!("invalid UTF-8 in KML: {err}")))?;
    let document =
        Document::parse(xml).map_err(|err| AppError::Parse(format!("invalid KML: {err}")))?;

    let mut rows = Vec::new();
    for placemark in document
        .descendants()
        .filter(|node| node.tag_name().name() == "Placemark")
    {
        if let Some(row) = parse_placemark(placemark)? {
            rows.push(row);
        }
    }

    Ok(rows)
}

pub fn persist_rows(
    connection: &mut Connection,
    project_id: i64,
    slot: ListSlot,
    drive_file_id: &str,
    rows: &[NormalizedRow],
) -> AppResult<ImportSummary> {
    let tx = connection.transaction()?;
    let list_name = slot.display_name();
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM lists WHERE project_id = ?1 AND slot = ?2 LIMIT 1",
            (project_id, slot.as_tag()),
            |row| row.get(0),
        )
        .optional()?;

    let list_id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE lists
                SET drive_file_id = ?1,
                    imported_at = DATETIME('now'),
                    name = ?3
                WHERE id = ?2",
                (drive_file_id, id, list_name),
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO lists (project_id, slot, name, source, drive_file_id)
                VALUES (?1, ?2, ?3, 'drive_kml', ?4)",
                (project_id, slot.as_tag(), list_name, drive_file_id),
            )?;
            tx.last_insert_rowid()
        }
    };

    tx.execute("DELETE FROM raw_items WHERE list_id = ?1", [list_id])?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO raw_items (list_id, source_row_hash, raw_json) VALUES (?1, ?2, ?3)",
        )?;
        for row in rows {
            stmt.execute(params![
                list_id,
                row.source_hash(),
                serde_json::to_string(row)?
            ])?;
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
    rows: &[NormalizedRow],
) -> AppResult<()> {
    for row in rows {
        telemetry.record(
            "raw_row_hashed",
            serde_json::json!({
                "slot": slot.as_tag(),
                "place_hash": row.place_hash(),
            }),
        )?;
    }
    Ok(())
}

fn parse_placemark(node: Node<'_, '_>) -> AppResult<Option<NormalizedRow>> {
    let name = node
        .children()
        .find(|child| child.tag_name().name() == "name")
        .and_then(|child| child.text())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Untitled placemark".to_string());

    let description = node
        .children()
        .find(|child| child.tag_name().name() == "description")
        .and_then(|child| child.text())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    let coordinates = node
        .descendants()
        .find(|child| child.tag_name().name() == "coordinates")
        .and_then(|child| child.text())
        .map(|value| value.trim().to_string());

    let coords = match coordinates {
        Some(value) => value,
        None => return Ok(None),
    };

    let (longitude, latitude, altitude) = parse_coordinates(&coords)
        .ok_or_else(|| AppError::Parse("Placemark missing valid coordinates".into()))?;

    Ok(Some(NormalizedRow {
        title: name,
        description,
        longitude,
        latitude,
        altitude,
        place_id: extract_place_id(node),
        raw_coordinates: coords,
    }))
}

fn parse_coordinates(value: &str) -> Option<(f64, f64, Option<f64>)> {
    let entry = value.split_whitespace().next()?;
    let mut parts = entry.split(',');
    let longitude = parts.next()?.trim().parse().ok()?;
    let latitude = parts.next()?.trim().parse().ok()?;
    let altitude = parts.next().and_then(|v| v.trim().parse().ok());
    Some((longitude, latitude, altitude))
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
        let rows = parse_kml(SAMPLE_KML.as_bytes()).unwrap();
        assert_eq!(rows.len(), 2);
        let first = &rows[0];
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
        let rows = parse_kml(SAMPLE_KML.as_bytes()).unwrap();
        let project_id: i64 = conn
            .query_row(
                "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let summary =
            persist_rows(&mut conn, project_id, ListSlot::A, "drive-file", &rows).unwrap();
        assert_eq!(summary.row_count, 2);
        enqueue_place_hashes(&telemetry, ListSlot::A, &rows).unwrap();

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
