//! Durable SQLite store for the Brain memory graph.
//!
//! Mirrors the Onyx data layer (`backend/onyx/db/brain.py` plus the
//! `Memory`/`MemoryRelation`/`MemorySource` models): pages are flat rows,
//! relations are undirected edges stored once per unordered pair with
//! `low < high`, sources are typed citations, and settings are a single row
//! (the local store is single-user, standing in for Onyx's per-user columns).

use std::collections::HashMap;
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::types::{
    BrainSettings, BrainSettingsUpdate, MemoryCategory, MemoryGraph, MemoryGraphEdge,
    MemoryGraphNode, MemoryPage, MemorySource, MemorySourceType, NewPage, PageUpdate, RelatedPages,
    memory_title_for_content, truncate_chars,
};
use crate::{BrainError, Result};

/// Maximum stored source-label length (Onyx caps at 512).
const SOURCE_LABEL_MAX: usize = 512;

/// The durable Brain store. Open with [`BrainStore::open`] (file-backed,
/// persists across reopen) or [`BrainStore::open_in_memory`] (tests).
pub struct BrainStore {
    conn: Connection,
}

impl std::fmt::Debug for BrainStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrainStore").finish_non_exhaustive()
    }
}

impl BrainStore {
    /// Open (creating if needed) a store at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            // Lazily create the parent dir like the rest of the workspace's
            // sqlite-backed stores.
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path)?;
        Self::from_connection(conn)
    }

    /// Open an ephemeral in-memory store.
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL").ok();
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    // -----------------------------------------------------------------------
    // Page CRUD
    // -----------------------------------------------------------------------

    /// Create a page and return it. Title falls back to the first sentence of
    /// the text when absent (Onyx `memory_title_for_content`).
    pub fn create_page(&self, page: NewPage) -> Result<MemoryPage> {
        let title = memory_title_for_content(&page.memory_text, page.title.as_deref());
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO memory (title, memory_text, category, source, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                title,
                page.memory_text,
                page.category.as_str(),
                page.source,
                now.to_rfc3339(),
                now.to_rfc3339(),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_page(id)?.ok_or(BrainError::PageNotFound(id))
    }

    /// Fetch one page by id.
    pub fn get_page(&self, id: i64) -> Result<Option<MemoryPage>> {
        self.conn
            .query_row(
                "SELECT id, title, memory_text, category, source, created_at, updated_at
                 FROM memory WHERE id = ?1",
                params![id],
                row_to_page,
            )
            .optional()
            .map_err(Into::into)
    }

    /// List every page, newest-updated first (Onyx list ordering).
    pub fn list_pages(&self) -> Result<Vec<MemoryPage>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, memory_text, category, source, created_at, updated_at
             FROM memory ORDER BY updated_at DESC, id DESC",
        )?;
        let pages = stmt
            .query_map([], row_to_page)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(pages)
    }

    /// Patch a page; `None` fields stay untouched. Bumps `updated_at`.
    pub fn update_page(&self, id: i64, update: PageUpdate) -> Result<MemoryPage> {
        let current = self.get_page(id)?.ok_or(BrainError::PageNotFound(id))?;
        let memory_text = update.memory_text.unwrap_or(current.memory_text);
        let title = match update.title {
            Some(title) => memory_title_for_content(&memory_text, Some(&title)),
            None => current.title,
        };
        let category = update.category.unwrap_or(current.category);
        let source = update.source.or(current.source);
        let now = Utc::now();
        self.conn.execute(
            "UPDATE memory SET title = ?1, memory_text = ?2, category = ?3, source = ?4,
             updated_at = ?5 WHERE id = ?6",
            params![
                title,
                memory_text,
                category.as_str(),
                source,
                now.to_rfc3339(),
                id
            ],
        )?;
        self.get_page(id)?.ok_or(BrainError::PageNotFound(id))
    }

    /// Delete a page and (via cascade) its relations and sources.
    pub fn delete_page(&self, id: i64) -> Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM memory WHERE id = ?1", params![id])?;
        Ok(n > 0)
    }

    // -----------------------------------------------------------------------
    // Relations (undirected edges)
    // -----------------------------------------------------------------------

    /// Create an undirected edge between two pages.
    ///
    /// Parity with Onyx `add_memory_relation`: self-edges are rejected, both
    /// endpoints must exist, and adding an existing edge (either direction)
    /// is a no-op that still reports the edge as present.
    pub fn add_relation(&self, a: i64, b: i64) -> Result<bool> {
        if a == b {
            return Err(BrainError::InvalidRelation(a, b, "self-edge"));
        }
        if self.get_page(a)?.is_none() || self.get_page(b)?.is_none() {
            return Err(BrainError::InvalidRelation(a, b, "unknown page"));
        }
        let (low, high) = ordered_pair(a, b);
        self.conn.execute(
            "INSERT OR IGNORE INTO memory_relation (memory_id_low, memory_id_high, created_at)
             VALUES (?1, ?2, ?3)",
            params![low, high, Utc::now().to_rfc3339()],
        )?;
        Ok(true)
    }

    /// Remove the edge between two pages (either direction). Removing an
    /// absent edge succeeds, matching Onyx.
    pub fn remove_relation(&self, a: i64, b: i64) -> Result<bool> {
        let (low, high) = ordered_pair(a, b);
        self.conn.execute(
            "DELETE FROM memory_relation WHERE memory_id_low = ?1 AND memory_id_high = ?2",
            params![low, high],
        )?;
        Ok(true)
    }

    /// Ids of every page sharing an edge with `id`.
    pub fn related_page_ids(&self, id: i64) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT memory_id_low, memory_id_high FROM memory_relation
             WHERE memory_id_low = ?1 OR memory_id_high = ?1",
        )?;
        let ids = stmt
            .query_map(params![id], |row| {
                let low: i64 = row.get(0)?;
                let high: i64 = row.get(1)?;
                Ok(if low == id { high } else { low })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Neighbors of `id` grouped by category, each group newest-updated first
    /// (Onyx `get_related_memories` ordering + the API's grouped shape).
    pub fn related_pages(&self, id: i64) -> Result<RelatedPages> {
        let ids = self.related_page_ids(id)?;
        let mut grouped = RelatedPages {
            notes: Vec::new(),
            concepts: Vec::new(),
            entities: Vec::new(),
            workstreams: Vec::new(),
        };
        // Reuse list ordering (updated_at DESC) then bucket by category.
        for page in self.list_pages()? {
            if !ids.contains(&page.id) {
                continue;
            }
            match page.category {
                MemoryCategory::Notes => grouped.notes.push(page),
                MemoryCategory::Concepts => grouped.concepts.push(page),
                MemoryCategory::Entities => grouped.entities.push(page),
                MemoryCategory::Workstreams => grouped.workstreams.push(page),
            }
        }
        Ok(grouped)
    }

    // -----------------------------------------------------------------------
    // Sources (typed citations)
    // -----------------------------------------------------------------------

    /// Attach a citation to a page. The label is capped at 512 chars.
    pub fn add_source(
        &self,
        memory_id: i64,
        source_type: MemorySourceType,
        label: &str,
        source_id: Option<&str>,
        url: Option<&str>,
    ) -> Result<MemorySource> {
        if self.get_page(memory_id)?.is_none() {
            return Err(BrainError::PageNotFound(memory_id));
        }
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO memory_source (memory_id, source_type, source_id, label, url, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                memory_id,
                source_type.as_str(),
                source_id,
                truncate_chars(label, SOURCE_LABEL_MAX),
                url,
                now.to_rfc3339(),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        Ok(MemorySource {
            id,
            memory_id,
            source_type,
            source_id: source_id.map(str::to_owned),
            label: truncate_chars(label, SOURCE_LABEL_MAX),
            url: url.map(str::to_owned),
            created_at: now,
        })
    }

    /// All citations for a page, oldest first (Onyx `get_memory_sources`).
    pub fn sources(&self, memory_id: i64) -> Result<Vec<MemorySource>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memory_id, source_type, source_id, label, url, created_at
             FROM memory_source WHERE memory_id = ?1 ORDER BY created_at, id",
        )?;
        let sources = stmt
            .query_map(params![memory_id], row_to_source)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(sources)
    }

    // -----------------------------------------------------------------------
    // Graph query
    // -----------------------------------------------------------------------

    /// Every page as a node (degree = touching edge count; degree-0 nodes are
    /// kept) plus the undirected edges, mirroring Onyx `get_memory_graph`.
    pub fn graph(&self) -> Result<MemoryGraph> {
        let pages = self.list_pages()?;
        if pages.is_empty() {
            return Ok(MemoryGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
            });
        }

        let mut stmt = self
            .conn
            .prepare("SELECT memory_id_low, memory_id_high FROM memory_relation")?;
        let edge_rows = stmt
            .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut degree: HashMap<i64, usize> = HashMap::new();
        for &(low, high) in &edge_rows {
            *degree.entry(low).or_default() += 1;
            *degree.entry(high).or_default() += 1;
        }

        let nodes = pages
            .iter()
            .map(|page| MemoryGraphNode {
                id: page.id,
                title: if page.title.is_empty() {
                    "Untitled memory".to_owned()
                } else {
                    page.title.clone()
                },
                category: page.category,
                degree: degree.get(&page.id).copied().unwrap_or(0),
                updated_at: page.updated_at,
            })
            .collect();
        let edges = edge_rows
            .into_iter()
            .map(|(low, high)| MemoryGraphEdge {
                source: low,
                target: high,
            })
            .collect();
        Ok(MemoryGraph { nodes, edges })
    }

    // -----------------------------------------------------------------------
    // Brain settings
    // -----------------------------------------------------------------------

    /// Current settings (defaults when never written: disabled, no connectors,
    /// no focus text, never run — the Onyx column defaults).
    pub fn settings(&self) -> Result<BrainSettings> {
        self.conn
            .query_row(
                "SELECT enabled, use_connectors, focus_instructions, last_run_at
                 FROM brain_settings WHERE id = 1",
                [],
                |row| {
                    Ok(BrainSettings {
                        enabled: row.get::<_, i64>(0)? != 0,
                        use_connectors: row.get::<_, i64>(1)? != 0,
                        focus_instructions: row.get(2)?,
                        last_run_at: parse_opt_ts(row.get::<_, Option<String>>(3)?),
                    })
                },
            )
            .optional()
            .map(Option::unwrap_or_default)
            .map_err(Into::into)
    }

    /// Patch settings; omitted fields stay untouched. `focus_instructions:
    /// Some(None)` (or all-whitespace text) clears the focus text, matching
    /// Onyx `update_brain_settings`.
    pub fn update_settings(&self, update: BrainSettingsUpdate) -> Result<BrainSettings> {
        let current = self.settings()?;
        let next = BrainSettings {
            enabled: update.enabled.unwrap_or(current.enabled),
            use_connectors: update.use_connectors.unwrap_or(current.use_connectors),
            focus_instructions: match update.focus_instructions {
                Some(value) => value.and_then(|text| {
                    let trimmed = text.trim();
                    (!trimmed.is_empty()).then(|| trimmed.to_owned())
                }),
                None => current.focus_instructions,
            },
            last_run_at: current.last_run_at,
        };
        self.write_settings(&next)?;
        Ok(next)
    }

    /// Record a completed run at `run_at` (Onyx `mark_brain_run_complete`).
    pub fn mark_run_complete(&self, run_at: DateTime<Utc>) -> Result<()> {
        let mut settings = self.settings()?;
        settings.last_run_at = Some(run_at);
        self.write_settings(&settings)
    }

    fn write_settings(&self, settings: &BrainSettings) -> Result<()> {
        self.conn.execute(
            "INSERT INTO brain_settings (id, enabled, use_connectors, focus_instructions, last_run_at)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 enabled = excluded.enabled,
                 use_connectors = excluded.use_connectors,
                 focus_instructions = excluded.focus_instructions,
                 last_run_at = excluded.last_run_at",
            params![
                settings.enabled as i64,
                settings.use_connectors as i64,
                settings.focus_instructions,
                settings.last_run_at.map(|ts| ts.to_rfc3339()),
            ],
        )?;
        Ok(())
    }
}

/// Normalize an unordered pair to `(low, high)` — the storage invariant that
/// makes edge dedup fall out of the primary key (Onyx `_ordered_pair`).
fn ordered_pair(a: i64, b: i64) -> (i64, i64) {
    if a < b { (a, b) } else { (b, a) }
}

fn row_to_page(row: &Row<'_>) -> rusqlite::Result<MemoryPage> {
    let category_raw: String = row.get(3)?;
    Ok(MemoryPage {
        id: row.get(0)?,
        title: row.get(1)?,
        memory_text: row.get(2)?,
        category: MemoryCategory::parse(&category_raw).unwrap_or(MemoryCategory::Notes),
        source: row.get(4)?,
        created_at: parse_ts(row.get::<_, String>(5)?),
        updated_at: parse_ts(row.get::<_, String>(6)?),
    })
}

fn row_to_source(row: &Row<'_>) -> rusqlite::Result<MemorySource> {
    let type_raw: String = row.get(2)?;
    Ok(MemorySource {
        id: row.get(0)?,
        memory_id: row.get(1)?,
        source_type: MemorySourceType::parse(&type_raw).unwrap_or(MemorySourceType::Manual),
        source_id: row.get(3)?,
        label: row.get(4)?,
        url: row.get(5)?,
        created_at: parse_ts(row.get::<_, String>(6)?),
    })
}

fn parse_ts(raw: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&raw)
        .map(|ts| ts.with_timezone(&Utc))
        .unwrap_or_default()
}

fn parse_opt_ts(raw: Option<String>) -> Option<DateTime<Utc>> {
    raw.map(parse_ts)
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memory (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    title TEXT NOT NULL,
    memory_text TEXT NOT NULL,
    category TEXT NOT NULL,
    source TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS memory_relation (
    memory_id_low INTEGER NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
    memory_id_high INTEGER NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    PRIMARY KEY (memory_id_low, memory_id_high),
    CHECK (memory_id_low < memory_id_high)
);
CREATE INDEX IF NOT EXISTS ix_memory_relation_high ON memory_relation(memory_id_high);

CREATE TABLE IF NOT EXISTS memory_source (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id INTEGER NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL,
    source_id TEXT,
    label TEXT NOT NULL,
    url TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_memory_source_memory ON memory_source(memory_id);

CREATE TABLE IF NOT EXISTS brain_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    enabled INTEGER NOT NULL DEFAULT 0,
    use_connectors INTEGER NOT NULL DEFAULT 0,
    focus_instructions TEXT,
    last_run_at TEXT
);
";
