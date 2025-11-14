use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use crate::errors::{AppError, AppResult};

#[derive(Debug, Serialize, Clone)]
pub struct ComparisonProjectRecord {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub created_at: String,
    pub updated_at: String,
    pub is_active: bool,
    pub list_a_imported_at: Option<String>,
    pub list_b_imported_at: Option<String>,
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
            la.imported_at AS list_a_imported_at,
            lb.imported_at AS list_b_imported_at
        FROM comparison_projects cp
        LEFT JOIN lists la ON la.project_id = cp.id AND la.slot = 'A'
        LEFT JOIN lists lb ON lb.project_id = cp.id AND lb.slot = 'B'
        ORDER BY cp.created_at ASC",
    )?;
    let rows = stmt
        .query_map([], |row| Ok(project_from_row(row)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn project_by_id(connection: &Connection, project_id: i64) -> AppResult<ComparisonProjectRecord> {
    connection
        .query_row(
            "SELECT
                cp.id,
                cp.name,
                cp.slug,
                cp.created_at,
                cp.updated_at,
                cp.is_active,
                la.imported_at AS list_a_imported_at,
                lb.imported_at AS list_b_imported_at
            FROM comparison_projects cp
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

fn unique_slug(connection: &Connection, name: &str) -> AppResult<String> {
    let base = slugify(name);
    let mut candidate = base.clone();
    let mut counter = 1;
    while slug_exists(connection, &candidate)? {
        counter += 1;
        candidate = format!("{base}-{counter}");
    }
    Ok(candidate)
}

fn slug_exists(connection: &Connection, slug: &str) -> AppResult<bool> {
    connection
        .query_row(
            "SELECT 1 FROM comparison_projects WHERE slug = ?1 LIMIT 1",
            [slug],
            |_| Ok(()),
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
    let is_active: i64 = row.get(5).unwrap_or(0);
    ComparisonProjectRecord {
        id: row.get(0).unwrap_or_default(),
        name: row.get(1).unwrap_or_default(),
        slug: row.get(2).unwrap_or_default(),
        created_at: row.get(3).unwrap_or_default(),
        updated_at: row.get(4).unwrap_or_default(),
        is_active: is_active == 1,
        list_a_imported_at: row.get(6).unwrap_or(None),
        list_b_imported_at: row.get(7).unwrap_or(None),
    }
}
