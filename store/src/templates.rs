//! Templates: a saved body the composer drops into a message. They belong
//! to the person rather than to one account, so no row here names one.

use rusqlite::{Connection, Row, params};

use crate::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub id: i64,
    pub name: String,
    /// The subject a message takes when it has none of its own. Empty when
    /// the template says nothing about it.
    pub subject: String,
    /// The body as Markdown, which is what a rich body reads and writes.
    pub markdown: String,
}

const COLUMNS: &str = "id, name, subject, markdown";

fn to_template(row: &Row<'_>) -> rusqlite::Result<Template> {
    Ok(Template {
        id: row.get(0)?,
        name: row.get(1)?,
        subject: row.get(2)?,
        markdown: row.get(3)?,
    })
}

/// Saves a new template and gives back its id. The one on `template` is
/// ignored, since SQLite hands out the row's own.
pub fn add(conn: &Connection, template: &Template) -> Result<i64> {
    conn.execute(
        "INSERT INTO templates (name, subject, markdown) VALUES (?1, ?2, ?3)",
        params![template.name, template.subject, template.markdown],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Writes a rename or an edit over the row `template` names.
pub fn update(conn: &Connection, template: &Template) -> Result<()> {
    conn.execute(
        "UPDATE templates SET name = ?2, subject = ?3, markdown = ?4 WHERE id = ?1",
        params![
            template.id,
            template.name,
            template.subject,
            template.markdown
        ],
    )?;
    Ok(())
}

/// Every template, by name, whichever case it was typed in.
pub fn list(conn: &Connection) -> Result<Vec<Template>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM templates ORDER BY name COLLATE NOCASE, id"
    ))?;
    let rows = stmt.query_map([], to_template)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn remove(conn: &Connection, id: i64) -> Result<()> {
    conn.execute("DELETE FROM templates WHERE id = ?1", params![id])?;
    Ok(())
}
