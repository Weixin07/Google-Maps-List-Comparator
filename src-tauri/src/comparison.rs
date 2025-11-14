use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::errors::{AppError, AppResult};
use crate::ingestion::ListSlot;

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonSnapshot {
    pub project: ComparisonProjectInfo,
    pub stats: ComparisonStats,
    pub overlap: Vec<PlaceComparisonRow>,
    pub only_a: Vec<PlaceComparisonRow>,
    pub only_b: Vec<PlaceComparisonRow>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonProjectInfo {
    pub id: i64,
    pub name: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonStats {
    pub list_a_count: usize,
    pub list_b_count: usize,
    pub overlap_count: usize,
    pub only_a_count: usize,
    pub only_b_count: usize,
    pub pending_a: usize,
    pub pending_b: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct PlaceComparisonRow {
    pub place_id: String,
    pub name: String,
    pub formatted_address: Option<String>,
    pub lat: f64,
    pub lng: f64,
    pub types: Vec<String>,
    pub lists: Vec<ListSlot>,
}

#[derive(Debug, Clone, Copy)]
pub enum ComparisonSegment {
    Overlap,
    OnlyA,
    OnlyB,
}

impl ComparisonSegment {
    pub fn as_str(&self) -> &'static str {
        match self {
            ComparisonSegment::Overlap => "overlap",
            ComparisonSegment::OnlyA => "only_a",
            ComparisonSegment::OnlyB => "only_b",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "overlap" => Some(ComparisonSegment::Overlap),
            "only_a" => Some(ComparisonSegment::OnlyA),
            "only_b" => Some(ComparisonSegment::OnlyB),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct PlaceEntry {
    place_id: String,
    name: String,
    formatted_address: Option<String>,
    lat: f64,
    lng: f64,
    types: Vec<String>,
}

impl PlaceEntry {
    fn into_row(self, lists: Vec<ListSlot>) -> PlaceComparisonRow {
        PlaceComparisonRow {
            place_id: self.place_id,
            name: self.name,
            formatted_address: self.formatted_address,
            lat: self.lat,
            lng: self.lng,
            types: self.types,
            lists,
        }
    }
}

pub fn compute_snapshot(conn: &Connection, project_id: i64) -> AppResult<ComparisonSnapshot> {
    let project = project_info(conn, project_id)?;
    let list_a = list_id(conn, project_id, ListSlot::A)?;
    let list_b = list_id(conn, project_id, ListSlot::B)?;

    let overlap = load_overlap(conn, list_a, list_b)?;
    let only_a = load_only(conn, list_a, list_b, ListSlot::A)?;
    let only_b = load_only(conn, list_b, list_a, ListSlot::B)?;

    let stats = ComparisonStats {
        list_a_count: count_places(conn, list_a)?,
        list_b_count: count_places(conn, list_b)?,
        overlap_count: overlap.len(),
        only_a_count: only_a.len(),
        only_b_count: only_b.len(),
        pending_a: pending_count(conn, list_a)?,
        pending_b: pending_count(conn, list_b)?,
    };

    Ok(ComparisonSnapshot {
        project,
        stats,
        overlap,
        only_a,
        only_b,
    })
}

fn project_info(conn: &Connection, project_id: i64) -> AppResult<ComparisonProjectInfo> {
    conn.query_row(
        "SELECT id, name FROM comparison_projects WHERE id = ?1 LIMIT 1",
        [project_id],
        |row| {
            Ok(ComparisonProjectInfo {
                id: row.get(0)?,
                name: row.get(1)?,
            })
        },
    )
    .map_err(AppError::from)
}

fn list_id(conn: &Connection, project_id: i64, slot: ListSlot) -> AppResult<Option<i64>> {
    conn.query_row(
        "SELECT id FROM lists WHERE project_id = ?1 AND slot = ?2 LIMIT 1",
        (project_id, slot.as_tag()),
        |row| row.get(0),
    )
    .optional()
    .map_err(AppError::from)
}

fn pending_count(conn: &Connection, list_id: Option<i64>) -> AppResult<usize> {
    let Some(list_id) = list_id else {
        return Ok(0);
    };
    conn.query_row(
        "SELECT COUNT(*)
        FROM raw_items ri
        LEFT JOIN normalization_cache nc ON nc.source_row_hash = ri.source_row_hash
        WHERE ri.list_id = ?1 AND nc.place_id IS NULL",
        [list_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value as usize)
    .map_err(AppError::from)
}

fn decode_types(value: Option<String>) -> Vec<String> {
    value
        .and_then(|text| serde_json::from_str::<Vec<String>>(&text).ok())
        .unwrap_or_default()
}

fn count_places(conn: &Connection, list_id: Option<i64>) -> AppResult<usize> {
    let Some(list_id) = list_id else {
        return Ok(0);
    };
    conn.query_row(
        "SELECT COUNT(*) FROM list_places WHERE list_id = ?1",
        [list_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value as usize)
    .map_err(AppError::from)
}

fn load_overlap(
    conn: &Connection,
    list_a: Option<i64>,
    list_b: Option<i64>,
) -> AppResult<Vec<PlaceComparisonRow>> {
    match (list_a, list_b) {
        (Some(a), Some(b)) => {
            let mut stmt = conn.prepare(
                "SELECT p.place_id, p.name, p.formatted_address, p.lat, p.lng, p.types
                FROM list_places lp
                JOIN places p ON p.place_id = lp.place_id
                WHERE lp.list_id = ?1
                AND EXISTS (
                    SELECT 1 FROM list_places other
                    WHERE other.list_id = ?2 AND other.place_id = lp.place_id
                )
                ORDER BY p.name COLLATE NOCASE",
            )?;
            let rows = stmt.query_map((a, b), |row| {
                Ok(PlaceEntry {
                    place_id: row.get(0)?,
                    name: row.get(1)?,
                    formatted_address: row.get(2)?,
                    lat: row.get(3)?,
                    lng: row.get(4)?,
                    types: decode_types(row.get(5)?),
                })
            })?;
            let mut results = Vec::new();
            for entry in rows {
                results.push(entry?.into_row(vec![ListSlot::A, ListSlot::B]));
            }
            Ok(results)
        }
        _ => Ok(Vec::new()),
    }
}

fn load_only(
    conn: &Connection,
    primary: Option<i64>,
    secondary: Option<i64>,
    slot: ListSlot,
) -> AppResult<Vec<PlaceComparisonRow>> {
    let Some(primary_id) = primary else {
        return Ok(Vec::new());
    };
    let (sql, params) = if let Some(secondary_id) = secondary {
        (
            "SELECT p.place_id, p.name, p.formatted_address, p.lat, p.lng, p.types
            FROM list_places lp
            JOIN places p ON p.place_id = lp.place_id
            WHERE lp.list_id = ?1
            AND NOT EXISTS (
                SELECT 1 FROM list_places other
                WHERE other.list_id = ?2 AND other.place_id = lp.place_id
            )
            ORDER BY p.name COLLATE NOCASE",
            (primary_id, secondary_id),
        )
    } else {
        (
            "SELECT p.place_id, p.name, p.formatted_address, p.lat, p.lng, p.types
            FROM list_places lp
            JOIN places p ON p.place_id = lp.place_id
            WHERE lp.list_id = ?1
            ORDER BY p.name COLLATE NOCASE",
            (primary_id, 0),
        )
    };
    let mut stmt = conn.prepare(sql)?;
    let mapper = |row: &Row<'_>| parse_place_entry(row);
    let rows = if secondary.is_some() {
        stmt.query_map(params, mapper)?
    } else {
        stmt.query_map([primary_id], mapper)?
    };
    let mut results = Vec::new();
    for entry in rows {
        results.push(entry?.into_row(vec![slot]));
    }
    Ok(results)
}

fn parse_place_entry(row: &Row<'_>) -> rusqlite::Result<PlaceEntry> {
    Ok(PlaceEntry {
        place_id: row.get(0)?,
        name: row.get(1)?,
        formatted_address: row.get(2)?,
        lat: row.get(3)?,
        lng: row.get(4)?,
        types: decode_types(row.get(5)?),
    })
}

impl ComparisonSnapshot {
    pub fn rows_for_segment(&self, segment: ComparisonSegment) -> &[PlaceComparisonRow] {
        match segment {
            ComparisonSegment::Overlap => &self.overlap,
            ComparisonSegment::OnlyA => &self.only_a,
            ComparisonSegment::OnlyB => &self.only_b,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::tempdir;

    use crate::db::bootstrap;
    use crate::secrets::SecretVault;

    use super::*;

    #[test]
    fn computes_overlap_and_only_sets() {
        let dir = tempdir().unwrap();
        let vault = SecretVault::in_memory();
        let bootstrap = bootstrap(dir.path(), "compare.db", &vault).unwrap();
        let conn = Arc::new(bootstrap.context.connection);

        let project_id: i64 = conn
            .as_ref()
            .query_row(
                "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap();

        {
            let conn_guard = conn.as_ref();
            conn_guard
                .execute(
                    "INSERT INTO lists (project_id, slot, name, source)
                     VALUES (?1, 'A', 'List A', 'test'), (?1, 'B', 'List B', 'test')",
                    [project_id],
                )
                .unwrap();
            let list_a_id: i64 = conn_guard
                .query_row(
                    "SELECT id FROM lists WHERE project_id = ?1 AND slot = 'A' LIMIT 1",
                    [project_id],
                    |row| row.get(0),
                )
                .unwrap();
            let list_b_id: i64 = conn_guard
                .query_row(
                    "SELECT id FROM lists WHERE project_id = ?1 AND slot = 'B' LIMIT 1",
                    [project_id],
                    |row| row.get(0),
                )
                .unwrap();
            conn_guard
                .execute(
                    "INSERT INTO places (place_id, name, formatted_address, lat, lng, types, last_checked_at)
                     VALUES
                        ('place_1','Alpha','Addr 1',1.0,1.0,'[\"park\"]',DATETIME('now')),
                        ('place_2','Bravo','Addr 2',2.0,2.0,'[\"cafe\"]',DATETIME('now')),
                        ('place_3','Charlie','Addr 3',3.0,3.0,'[\"museum\"]',DATETIME('now'))",
                    [],
                )
                .unwrap();

            conn_guard
                .execute(
                    "INSERT INTO list_places (list_id, place_id, assigned_at)
                     VALUES
                        (?1,'place_1',DATETIME('now')),
                        (?1,'place_2',DATETIME('now')),
                        (?2,'place_2',DATETIME('now')),
                        (?2,'place_3',DATETIME('now'))",
                    (list_a_id, list_b_id),
                )
                .unwrap();

            conn_guard
                .execute(
                    "INSERT INTO raw_items (list_id, source_row_hash, raw_json)
                     VALUES
                        (?1,'hash_a','{}'),
                        (?2,'hash_b','{}')",
                    (list_a_id, list_b_id),
                )
                .unwrap();
        }

        let snapshot = compute_snapshot(conn.as_ref(), project_id).unwrap();
        assert_eq!(snapshot.project.id, project_id);
        assert_eq!(snapshot.stats.overlap_count, 1);
        assert_eq!(snapshot.stats.only_a_count, 1);
        assert_eq!(snapshot.stats.only_b_count, 1);
        assert_eq!(snapshot.overlap[0].place_id, "place_2");
        assert_eq!(snapshot.only_a[0].place_id, "place_1");
        assert_eq!(snapshot.only_b[0].place_id, "place_3");
    }
}
