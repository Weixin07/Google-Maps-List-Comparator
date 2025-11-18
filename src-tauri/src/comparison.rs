use std::cmp;

use rusqlite::{Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::errors::{AppError, AppResult};
use crate::ingestion::ListSlot;

const DEFAULT_PAGE_SIZE: usize = 200;
const MAX_PAGE_SIZE: usize = 1000;

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonSnapshot {
    pub project: ComparisonProjectInfo,
    pub stats: ComparisonStats,
    pub lists: ComparisonLists,
    pub overlap: ComparisonSegmentPage,
    pub only_a: ComparisonSegmentPage,
    pub only_b: ComparisonSegmentPage,
}

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonSegmentPage {
    pub rows: Vec<PlaceComparisonRow>,
    pub total: usize,
    pub page: usize,
    pub page_size: usize,
}

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonLists {
    pub list_a_id: Option<i64>,
    pub list_b_id: Option<i64>,
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

#[derive(Debug, Clone, Copy)]
pub struct ComparisonPagination {
    pub page: usize,
    pub page_size: usize,
}

impl ComparisonPagination {
    pub fn new(page: Option<usize>, page_size: Option<usize>) -> Self {
        let sanitized_page_size = page_size
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        let sanitized_page = page.unwrap_or(1).max(1);
        Self {
            page: sanitized_page,
            page_size: sanitized_page_size,
        }
    }

    pub fn with_total(self, total: usize) -> Self {
        if total == 0 {
            return Self {
                page: 1,
                page_size: self.page_size,
            };
        }
        let pages = (total + self.page_size - 1) / self.page_size;
        let capped_page = cmp::min(self.page, pages);
        Self {
            page: capped_page.max(1),
            page_size: self.page_size,
        }
    }

    fn offset(&self) -> i64 {
        self.page.saturating_sub(1).saturating_mul(self.page_size) as i64
    }
}

impl Default for ComparisonPagination {
    fn default() -> Self {
        Self {
            page: 1,
            page_size: DEFAULT_PAGE_SIZE,
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

pub fn compute_snapshot(
    conn: &Connection,
    project_id: i64,
    pagination: Option<ComparisonPagination>,
) -> AppResult<ComparisonSnapshot> {
    let project = project_info(conn, project_id)?;
    let list_a = list_id(conn, project_id, ListSlot::A)?;
    let list_b = list_id(conn, project_id, ListSlot::B)?;
    let stats = ComparisonStats {
        list_a_count: count_places(conn, list_a)?,
        list_b_count: count_places(conn, list_b)?,
        overlap_count: count_segment(conn, project_id, ComparisonSegment::Overlap)?,
        only_a_count: count_segment(conn, project_id, ComparisonSegment::OnlyA)?,
        only_b_count: count_segment(conn, project_id, ComparisonSegment::OnlyB)?,
        pending_a: pending_count(conn, list_a)?,
        pending_b: pending_count(conn, list_b)?,
    };

    let overlap_page = pagination.map(|p| p.with_total(stats.overlap_count));
    let only_a_page = pagination.map(|p| p.with_total(stats.only_a_count));
    let only_b_page = pagination.map(|p| p.with_total(stats.only_b_count));
    let overlap = load_segment(conn, project_id, ComparisonSegment::Overlap, overlap_page)?;
    let only_a = load_segment(conn, project_id, ComparisonSegment::OnlyA, only_a_page)?;
    let only_b = load_segment(conn, project_id, ComparisonSegment::OnlyB, only_b_page)?;

    Ok(ComparisonSnapshot {
        project,
        stats,
        lists: ComparisonLists {
            list_a_id: list_a,
            list_b_id: list_b,
        },
        overlap,
        only_a,
        only_b,
    })
}

pub fn load_segment_page(
    conn: &Connection,
    project_id: i64,
    segment: ComparisonSegment,
    pagination: ComparisonPagination,
) -> AppResult<ComparisonSegmentPage> {
    load_segment(conn, project_id, segment, Some(pagination))
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

fn count_segment(
    conn: &Connection,
    project_id: i64,
    segment: ComparisonSegment,
) -> AppResult<usize> {
    let table = segment_table(segment);
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE project_id = ?1");
    conn.query_row(&sql, [project_id], |row| row.get::<_, i64>(0))
        .map(|value| value as usize)
        .map_err(AppError::from)
}

fn load_segment(
    conn: &Connection,
    project_id: i64,
    segment: ComparisonSegment,
    pagination: Option<ComparisonPagination>,
) -> AppResult<ComparisonSegmentPage> {
    let total = count_segment(conn, project_id, segment)?;
    let lists = segment_lists(segment);
    let effective_pagination = pagination.map(|p| p.with_total(total));
    let table = segment_table(segment);
    let base_sql = format!(
        "SELECT place_id, name, formatted_address, lat, lng, types
        FROM {table}
        WHERE project_id = ?1
        ORDER BY name COLLATE NOCASE"
    );

    let mapper = |row: &Row<'_>| parse_place_entry(row);
    let rows = if let Some(paging) = effective_pagination {
        let limited = format!("{base_sql} LIMIT ?2 OFFSET ?3");
        let mut stmt = conn.prepare(&limited)?;
        let iter = stmt.query_map(
            (project_id, paging.page_size as i64, paging.offset()),
            mapper,
        )?;
        parse_segment_rows(iter, lists)
    } else {
        let mut stmt = conn.prepare(&base_sql)?;
        let iter = stmt.query_map([project_id], mapper)?;
        parse_segment_rows(iter, lists)
    }?;

    let (page, page_size) = effective_pagination
        .map(|p| (p.page, p.page_size))
        .unwrap_or_else(|| (1, cmp::max(total, 1)));

    Ok(ComparisonSegmentPage {
        rows,
        total,
        page,
        page_size,
    })
}

fn parse_segment_rows(
    rows: impl Iterator<Item = rusqlite::Result<PlaceEntry>>,
    lists: Vec<ListSlot>,
) -> AppResult<Vec<PlaceComparisonRow>> {
    let mut results = Vec::new();
    for entry in rows {
        results.push(entry?.into_row(lists.clone()));
    }
    Ok(results)
}

fn segment_table(segment: ComparisonSegment) -> &'static str {
    match segment {
        ComparisonSegment::Overlap => "comparison_overlap",
        ComparisonSegment::OnlyA => "comparison_only_a",
        ComparisonSegment::OnlyB => "comparison_only_b",
    }
}

fn segment_lists(segment: ComparisonSegment) -> Vec<ListSlot> {
    match segment {
        ComparisonSegment::Overlap => vec![ListSlot::A, ListSlot::B],
        ComparisonSegment::OnlyA => vec![ListSlot::A],
        ComparisonSegment::OnlyB => vec![ListSlot::B],
    }
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
            ComparisonSegment::Overlap => &self.overlap.rows,
            ComparisonSegment::OnlyA => &self.only_a.rows,
            ComparisonSegment::OnlyB => &self.only_b.rows,
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

        let snapshot = compute_snapshot(conn.as_ref(), project_id, None).unwrap();
        assert_eq!(snapshot.project.id, project_id);
        assert_eq!(snapshot.stats.overlap_count, 1);
        assert_eq!(snapshot.stats.only_a_count, 1);
        assert_eq!(snapshot.stats.only_b_count, 1);
        assert_eq!(snapshot.overlap.rows[0].place_id, "place_2");
        assert_eq!(snapshot.only_a.rows[0].place_id, "place_1");
        assert_eq!(snapshot.only_b.rows[0].place_id, "place_3");
    }
}
