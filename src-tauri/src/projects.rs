use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::comparison::ComparisonStats;
use crate::db;
use crate::errors::{AppError, AppResult};

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonProjectRecord {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub created_at: String,
    pub updated_at: String,
    pub is_active: bool,
    pub last_compared_at: Option<String>,
    pub list_a_id: Option<i64>,
    pub list_b_id: Option<i64>,
    pub list_a_imported_at: Option<String>,
    pub list_b_imported_at: Option<String>,
    pub list_a_drive_file: Option<DriveFileRecord>,
    pub list_b_drive_file: Option<DriveFileRecord>,
}

#[derive(Debug, Serialize, Clone)]
pub struct DriveFileRecord {
    pub id: String,
    pub name: String,
    pub mime_type: Option<String>,
    pub modified_time: Option<String>,
    pub size: Option<u64>,
    pub md5_checksum: Option<String>,
}

pub fn active_project_id(connection: &Connection) -> AppResult<i64> {
    connection
        .query_row(
            "SELECT id FROM comparison_projects WHERE is_active = 1 LIMIT 1",
            [],
            |row| row.get(0),
        )
        .map_err(AppError::from)
}

pub fn list_projects(connection: &Connection) -> AppResult<Vec<ComparisonProjectRecord>> {
    let mut stmt = connection.prepare(
        "SELECT
            cp.id,
            cp.name,
            cp.slug,
            cp.created_at,
            cp.updated_at,
            cp.is_active,
            COALESCE(cp.last_compared_at, lr.last_compared_at) AS last_compared_at,
            la.id AS list_a_id,
            lb.id AS list_b_id,
            la.imported_at AS list_a_imported_at,
            lb.imported_at AS list_b_imported_at,
            la.drive_file_id AS list_a_drive_file_id,
            la.drive_file_name AS list_a_drive_file_name,
            la.drive_file_mime AS list_a_drive_file_mime,
            la.drive_file_size AS list_a_drive_file_size,
            la.drive_modified_time AS list_a_drive_modified_time,
            la.drive_file_checksum AS list_a_drive_checksum,
            lb.drive_file_id AS list_b_drive_file_id,
            lb.drive_file_name AS list_b_drive_file_name,
            lb.drive_file_mime AS list_b_drive_file_mime,
            lb.drive_file_size AS list_b_drive_file_size,
            lb.drive_modified_time AS list_b_drive_modified_time,
            lb.drive_file_checksum AS list_b_drive_checksum
        FROM comparison_projects cp
        LEFT JOIN (
            SELECT project_id, MAX(completed_at) AS last_compared_at
            FROM comparison_runs
            GROUP BY project_id
        ) AS lr ON lr.project_id = cp.id
        LEFT JOIN lists la ON la.project_id = cp.id AND la.slot = 'A'
        LEFT JOIN lists lb ON lb.project_id = cp.id AND lb.slot = 'B'
        ORDER BY cp.created_at ASC",
    )?;
    let rows = stmt
        .query_map([], |row| Ok(project_from_row(row)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn project_by_id(
    connection: &Connection,
    project_id: i64,
) -> AppResult<ComparisonProjectRecord> {
    connection
        .query_row(
            "SELECT
                cp.id,
                cp.name,
                cp.slug,
                cp.created_at,
                cp.updated_at,
                cp.is_active,
                COALESCE(cp.last_compared_at, lr.last_compared_at) AS last_compared_at,
                la.id AS list_a_id,
                lb.id AS list_b_id,
                la.imported_at AS list_a_imported_at,
                lb.imported_at AS list_b_imported_at,
                la.drive_file_id AS list_a_drive_file_id,
                la.drive_file_name AS list_a_drive_file_name,
                la.drive_file_mime AS list_a_drive_file_mime,
                la.drive_file_size AS list_a_drive_file_size,
                la.drive_modified_time AS list_a_drive_modified_time,
                la.drive_file_checksum AS list_a_drive_checksum,
                lb.drive_file_id AS list_b_drive_file_id,
                lb.drive_file_name AS list_b_drive_file_name,
                lb.drive_file_mime AS list_b_drive_file_mime,
                lb.drive_file_size AS list_b_drive_file_size,
                lb.drive_modified_time AS list_b_drive_modified_time,
                lb.drive_file_checksum AS list_b_drive_checksum
            FROM comparison_projects cp
            LEFT JOIN (
                SELECT project_id, MAX(completed_at) AS last_compared_at
                FROM comparison_runs
                GROUP BY project_id
            ) AS lr ON lr.project_id = cp.id
            LEFT JOIN lists la ON la.project_id = cp.id AND la.slot = 'A'
            LEFT JOIN lists lb ON lb.project_id = cp.id AND lb.slot = 'B'
            WHERE cp.id = ?1
            LIMIT 1",
            [project_id],
            |row| Ok(project_from_row(row)),
        )
        .map_err(AppError::from)
}

pub fn create_project(
    connection: &Connection,
    name: &str,
    activate: bool,
) -> AppResult<ComparisonProjectRecord> {
    let normalized_name = name.trim();
    if normalized_name.is_empty() {
        return Err(AppError::Config("project name cannot be empty".into()));
    }
    let slug = unique_slug(connection, normalized_name)?;
    if activate {
        connection.execute(
            "UPDATE comparison_projects SET is_active = 0 WHERE is_active = 1",
            [],
        )?;
    }
    connection.execute(
        "INSERT INTO comparison_projects (name, slug, is_active)
        VALUES (?1, ?2, ?3)",
        params![normalized_name, slug, if activate { 1 } else { 0 }],
    )?;
    let id = connection.last_insert_rowid();
    project_by_id(connection, id)
}

pub fn rename_project(
    connection: &Connection,
    project_id: i64,
    name: &str,
) -> AppResult<ComparisonProjectRecord> {
    let normalized = name.trim();
    if normalized.is_empty() {
        return Err(AppError::Config("project name cannot be empty".into()));
    }

    let existing = project_by_id(connection, project_id)?;
    if existing.name == normalized {
        return Ok(existing);
    }

    let slug = unique_slug_excluding(connection, normalized, Some(project_id))?;
    connection.execute(
        "UPDATE comparison_projects
        SET name = ?1, slug = ?2, updated_at = DATETIME('now')
        WHERE id = ?3",
        (normalized, slug, project_id),
    )?;
    project_by_id(connection, project_id)
}

pub fn set_active_project(connection: &Connection, project_id: i64) -> AppResult<()> {
    let affected = connection.execute(
        "UPDATE comparison_projects
        SET is_active = CASE WHEN id = ?1 THEN 1 ELSE 0 END,
            updated_at = DATETIME('now')
        WHERE id IN (SELECT id FROM comparison_projects)",
        [project_id],
    )?;
    if affected == 0 {
        return Err(AppError::Config(format!(
            "comparison project {project_id} not found"
        )));
    }
    Ok(())
}

pub fn record_comparison_run(
    connection: &Connection,
    project_id: i64,
    list_a_id: Option<i64>,
    list_b_id: Option<i64>,
    stats: &ComparisonStats,
    started_at: String,
    duration_ms: u128,
) -> AppResult<()> {
    let completed_at = db::now_timestamp();
    connection.execute(
        "INSERT INTO comparison_runs (
            project_id,
            list_a_id,
            list_b_id,
            list_a_count,
            list_b_count,
            overlap_count,
            only_a_count,
            only_b_count,
            pending_a,
            pending_b,
            duration_ms,
            started_at,
            completed_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            project_id,
            list_a_id,
            list_b_id,
            stats.list_a_count as i64,
            stats.list_b_count as i64,
            stats.overlap_count as i64,
            stats.only_a_count as i64,
            stats.only_b_count as i64,
            stats.pending_a as i64,
            stats.pending_b as i64,
            duration_ms.min(i64::MAX as u128) as i64,
            started_at,
            completed_at
        ],
    )?;
    connection.execute(
        "UPDATE comparison_projects
        SET last_compared_at = ?1, updated_at = DATETIME('now')
        WHERE id = ?2",
        (&completed_at, project_id),
    )?;
    Ok(())
}

fn unique_slug(connection: &Connection, name: &str) -> AppResult<String> {
    unique_slug_excluding(connection, name, None)
}

fn unique_slug_excluding(
    connection: &Connection,
    name: &str,
    exclude_project_id: Option<i64>,
) -> AppResult<String> {
    let base = slugify(name);
    let mut candidate = base.clone();
    let mut counter = 1;
    while slug_exists(connection, &candidate, exclude_project_id)? {
        counter += 1;
        candidate = format!("{base}-{counter}");
    }
    Ok(candidate)
}

fn slug_exists(
    connection: &Connection,
    slug: &str,
    exclude_project_id: Option<i64>,
) -> AppResult<bool> {
    connection
        .query_row(
            "SELECT 1 FROM comparison_projects WHERE slug = ?1 AND (?2 IS NULL OR id != ?2) LIMIT 1",
            (slug, exclude_project_id),
            |_| Ok::<(), rusqlite::Error>(()),
        )
        .optional()
        .map(|opt| opt.is_some())
        .map_err(AppError::from)
}

fn slugify(name: &str) -> String {
    let filtered = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let collapsed = filtered
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if collapsed.is_empty() {
        "project".into()
    } else {
        collapsed
    }
}

fn project_from_row(row: &Row<'_>) -> ComparisonProjectRecord {
    let is_active: i64 = row.get("is_active").unwrap_or(0);
    let list_a_drive_file = drive_file_from_row(row, "list_a_drive_file");
    let list_b_drive_file = drive_file_from_row(row, "list_b_drive_file");
    ComparisonProjectRecord {
        id: row.get("id").unwrap_or_default(),
        name: row.get("name").unwrap_or_default(),
        slug: row.get("slug").unwrap_or_default(),
        created_at: row.get("created_at").unwrap_or_default(),
        updated_at: row.get("updated_at").unwrap_or_default(),
        is_active: is_active == 1,
        last_compared_at: row.get("last_compared_at").unwrap_or(None),
        list_a_id: row.get("list_a_id").unwrap_or(None),
        list_b_id: row.get("list_b_id").unwrap_or(None),
        list_a_imported_at: row.get("list_a_imported_at").unwrap_or(None),
        list_b_imported_at: row.get("list_b_imported_at").unwrap_or(None),
        list_a_drive_file,
        list_b_drive_file,
    }
}

fn drive_file_from_row(row: &Row<'_>, alias_prefix: &str) -> Option<DriveFileRecord> {
    let drive_id_col = format!("{alias_prefix}_id");
    let name_col = format!("{alias_prefix}_name");
    let mime_col = format!("{alias_prefix}_mime");
    let size_col = format!("{alias_prefix}_size");
    let modified_col = format!("{alias_prefix}_modified_time");
    let checksum_col = format!("{alias_prefix}_checksum");

    let drive_id: Option<String> = row.get(drive_id_col.as_str()).unwrap_or(None);
    let name: Option<String> = row.get(name_col.as_str()).unwrap_or(None);
    let mime_type: Option<String> = row.get(mime_col.as_str()).unwrap_or(None);
    let size: Option<i64> = row.get(size_col.as_str()).unwrap_or(None);
    let modified_time: Option<String> = row.get(modified_col.as_str()).unwrap_or(None);
    let checksum: Option<String> = row.get(checksum_col.as_str()).unwrap_or(None);
    drive_id.map(|id| DriveFileRecord {
        name: name.unwrap_or_else(|| id.clone()),
        id,
        mime_type,
        modified_time,
        size: size.and_then(|value| value.try_into().ok()),
        md5_checksum: checksum,
    })
}
