//! Durable SQLite store for the Brain memory graph.
//!
//! Mirrors the Onyx data layer (`backend/onyx/db/brain.py` plus the
//! `Memory`/`MemoryRelation`/`MemorySource` models): pages are flat rows,
//! relations are undirected edges stored once per unordered pair with
//! `low < high`, sources are typed citations, and settings are a single row
//! (the local store is single-user, standing in for Onyx's per-user columns).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::types::{
    BrainSettings, BrainSettingsUpdate, BrainStatus, MemoryCategory, MemoryFreshness, MemoryGraph,
    MemoryGraphEdge, MemoryGraphNode, MemoryPage, MemoryRevision, MemoryScopeKind, MemorySource,
    MemorySourceType, NewPage, PageUpdate, RecallOptions, RecalledMemoryPage, RelatedPages,
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

impl BrainStore {
    /// Crate-internal connection access, so sibling modules (procedural
    /// memory, graph health) can add stores without reopening the database.
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
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
        migrate_schema(&conn)?;
        Ok(Self { conn })
    }

    // -----------------------------------------------------------------------
    // Page CRUD
    // -----------------------------------------------------------------------

    /// Create a global page and return it. Title falls back to the first sentence of
    /// the text when absent (Onyx `memory_title_for_content`).
    pub fn create_page(&self, page: NewPage) -> Result<MemoryPage> {
        self.create_page_scoped(page, None)
    }

    /// Create a page scoped to `scope_id` when provided; otherwise global.
    pub fn create_page_scoped(&self, page: NewPage, scope_id: Option<&str>) -> Result<MemoryPage> {
        let title = memory_title_for_content(&page.memory_text, page.title.as_deref());
        let now = Utc::now();
        let scope_kind = scope_id
            .filter(|scope| !scope.trim().is_empty())
            .map_or(MemoryScopeKind::Global, |_| MemoryScopeKind::Workspace);
        let scope_id = scope_id.and_then(normalize_scope_id);
        let freshness = freshness_for_source(page.source.as_deref());
        self.conn.execute(
            "INSERT INTO memory (title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                title,
                page.memory_text,
                page.category.as_str(),
                scope_kind.as_str(),
                scope_id,
                page.source,
                freshness.as_str(),
                now.to_rfc3339(),
                now.to_rfc3339(),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        let created = self.get_page(id)?.ok_or(BrainError::PageNotFound(id))?;
        self.record_revision(&created, "create")?;
        Ok(created)
    }

    /// Fetch one page by exact normalized title (case-insensitive), across scopes.
    pub fn get_page_by_title(&self, title: &str) -> Result<Option<MemoryPage>> {
        self.get_page_by_title_scoped(title, None, false)
    }

    /// Fetch one page by title in the requested scope. When `strict_scope` is
    /// false, workspace lookups may fall back to a global page with the same title.
    pub fn get_page_by_title_scoped(
        &self,
        title: &str,
        scope_id: Option<&str>,
        strict_scope: bool,
    ) -> Result<Option<MemoryPage>> {
        let key = title.trim().to_lowercase();
        let scope_id = scope_id.and_then(normalize_scope_id);
        let (sql, params): (&str, Vec<Box<dyn rusqlite::ToSql>>) = match (scope_id.as_deref(), strict_scope) {
            (Some(scope), true) => (
                "SELECT id, title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at
                 FROM memory WHERE lower(trim(title)) = ?1 AND scope_kind = 'workspace' AND scope_id = ?2
                 ORDER BY updated_at DESC, id DESC LIMIT 1",
                vec![Box::new(key), Box::new(scope.to_owned())],
            ),
            (Some(scope), false) => (
                "SELECT id, title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at
                 FROM memory WHERE lower(trim(title)) = ?1
                   AND ((scope_kind = 'workspace' AND scope_id = ?2) OR scope_kind = 'global')
                 ORDER BY CASE WHEN scope_kind = 'workspace' AND scope_id = ?2 THEN 0 ELSE 1 END,
                          updated_at DESC, id DESC LIMIT 1",
                vec![Box::new(key), Box::new(scope.to_owned())],
            ),
            (None, _) => (
                "SELECT id, title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at
                 FROM memory WHERE lower(trim(title)) = ?1 AND scope_kind = 'global'
                 ORDER BY updated_at DESC, id DESC LIMIT 1",
                vec![Box::new(key)],
            ),
        };
        let params_ref = params.iter().map(|p| p.as_ref()).collect::<Vec<_>>();
        self.conn
            .query_row(sql, rusqlite::params_from_iter(params_ref), row_to_page)
            .optional()
            .map_err(Into::into)
    }

    /// Create a global page or update the existing page with the same normalized title,
    /// matching Onyx Brain's create-or-update-by-title run behavior.
    pub fn create_or_update_page_by_title(&self, page: NewPage) -> Result<MemoryPage> {
        self.create_or_update_page_by_title_scoped(page, None)
    }

    /// Create or update a page within the requested scope.
    pub fn create_or_update_page_by_title_scoped(
        &self,
        page: NewPage,
        scope_id: Option<&str>,
    ) -> Result<MemoryPage> {
        let title = memory_title_for_content(&page.memory_text, page.title.as_deref());
        if let Some(existing) = self.get_page_by_title_scoped(&title, scope_id, true)? {
            return self.update_page(
                existing.id,
                PageUpdate {
                    title: Some(title),
                    memory_text: Some(page.memory_text),
                    category: Some(page.category),
                    source: page.source,
                    ..PageUpdate::default()
                },
            );
        }
        self.create_page_scoped(
            NewPage {
                title: Some(title),
                ..page
            },
            scope_id,
        )
    }

    /// Fetch one page by id.
    pub fn get_page(&self, id: i64) -> Result<Option<MemoryPage>> {
        self.conn
            .query_row(
                "SELECT id, title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at
                 FROM memory WHERE id = ?1",
                params![id],
                row_to_page,
            )
            .optional()
            .map_err(Into::into)
    }

    /// List every page, newest-updated first (Onyx list ordering).
    pub fn list_pages(&self) -> Result<Vec<MemoryPage>> {
        self.list_pages_inner(None)
    }

    /// List pages in one category, newest-updated first. This is the library
    /// equivalent of Onyx `/memory?category=...`.
    pub fn list_pages_by_category(&self, category: MemoryCategory) -> Result<Vec<MemoryPage>> {
        self.list_pages_inner(Some(category))
    }

    /// List global pages plus pages matching the active workspace scope.
    pub fn list_pages_for_scope(&self, scope_id: Option<&str>) -> Result<Vec<MemoryPage>> {
        let Some(scope_id) = scope_id.and_then(normalize_scope_id) else {
            return self.list_pages();
        };
        let mut stmt = self.conn.prepare(
            "SELECT id, title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at
             FROM memory WHERE scope_kind = 'global' OR (scope_kind = 'workspace' AND scope_id = ?1)
             ORDER BY CASE WHEN scope_kind = 'workspace' AND scope_id = ?1 THEN 0 ELSE 1 END,
                      updated_at DESC, id DESC",
        )?;
        stmt.query_map(params![scope_id], row_to_page)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    fn list_pages_inner(&self, category: Option<MemoryCategory>) -> Result<Vec<MemoryPage>> {
        let sql = if category.is_some() {
            "SELECT id, title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at
             FROM memory WHERE category = ?1 ORDER BY updated_at DESC, id DESC"
        } else {
            "SELECT id, title, memory_text, category, scope_kind, scope_id, source, freshness, created_at, updated_at
             FROM memory ORDER BY updated_at DESC, id DESC"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let pages = if let Some(category) = category {
            stmt.query_map(params![category.as_str()], row_to_page)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        } else {
            stmt.query_map([], row_to_page)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        Ok(pages)
    }

    /// Per-category page counts (the Onyx `/memory` list `category_counts`
    /// shape). Categories with no pages report 0.
    pub fn category_counts(&self) -> Result<BTreeMap<MemoryCategory, usize>> {
        let mut counts: BTreeMap<MemoryCategory, usize> =
            MemoryCategory::all().into_iter().map(|c| (c, 0)).collect();
        let mut stmt = self
            .conn
            .prepare("SELECT category, COUNT(*) FROM memory GROUP BY category")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        for (raw, count) in rows {
            let category = MemoryCategory::parse(&raw).unwrap_or(MemoryCategory::Notes);
            *counts.entry(category).or_default() += count as usize;
        }
        Ok(counts)
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
        let freshness = update.freshness.unwrap_or_else(|| match source.as_deref() {
            Some("current_state") | Some("git_state") | Some("sync_state") => {
                MemoryFreshness::TimeSensitive
            }
            _ => current.freshness,
        });
        let now = Utc::now();
        self.conn.execute(
            "UPDATE memory SET title = ?1, memory_text = ?2, category = ?3, source = ?4,
             freshness = ?5, updated_at = ?6 WHERE id = ?7",
            params![
                title,
                memory_text,
                category.as_str(),
                source,
                freshness.as_str(),
                now.to_rfc3339(),
                id
            ],
        )?;
        let updated = self.get_page(id)?.ok_or(BrainError::PageNotFound(id))?;
        self.record_revision(&updated, "update")?;
        Ok(updated)
    }

    /// Restore a page from a recorded revision and record the restored state.
    pub fn restore_revision(&self, revision_id: i64) -> Result<MemoryPage> {
        let revision = self
            .get_revision(revision_id)?
            .ok_or(BrainError::PageNotFound(revision_id))?;
        self.conn.execute(
            "UPDATE memory SET title = ?1, memory_text = ?2, category = ?3,
             scope_kind = ?4, scope_id = ?5, source = ?6, freshness = ?7, updated_at = ?8 WHERE id = ?9",
            params![
                revision.title,
                revision.memory_text,
                revision.category.as_str(),
                revision.scope_kind.as_str(),
                revision.scope_id,
                revision.source,
                revision.freshness.as_str(),
                Utc::now().to_rfc3339(),
                revision.memory_id,
            ],
        )?;
        let restored = self
            .get_page(revision.memory_id)?
            .ok_or(BrainError::PageNotFound(revision.memory_id))?;
        self.record_revision(&restored, "restore")?;
        Ok(restored)
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

    /// Attach a citation unless this page already has a source with the same
    /// source_id. If `source_id` is `None`, a new row is always added.
    pub fn add_source_if_missing(
        &self,
        memory_id: i64,
        source_type: MemorySourceType,
        label: &str,
        source_id: Option<&str>,
        url: Option<&str>,
    ) -> Result<Option<MemorySource>> {
        if let Some(source_id) = source_id
            && self
                .sources(memory_id)?
                .iter()
                .any(|source| source.source_id.as_deref() == Some(source_id))
        {
            return Ok(None);
        }
        self.add_source(memory_id, source_type, label, source_id, url)
            .map(Some)
    }

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
    // Revisions
    // -----------------------------------------------------------------------

    /// Record the current page state as a revision.
    pub fn record_revision(
        &self,
        page: &MemoryPage,
        revision_source: &str,
    ) -> Result<MemoryRevision> {
        let now = Utc::now();
        self.conn.execute(
            "INSERT INTO memory_revision
             (memory_id, title, memory_text, category, scope_kind, scope_id, source, freshness, revision_source, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                page.id,
                page.title,
                page.memory_text,
                page.category.as_str(),
                page.scope_kind.as_str(),
                page.scope_id,
                page.source,
                page.freshness.as_str(),
                revision_source,
                now.to_rfc3339(),
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_revision(id)?.ok_or(BrainError::PageNotFound(id))
    }

    /// List all revisions for a page, newest first.
    pub fn revisions(&self, memory_id: i64) -> Result<Vec<MemoryRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memory_id, title, memory_text, category, scope_kind, scope_id,
                    source, freshness, revision_source, created_at
             FROM memory_revision WHERE memory_id = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        stmt.query_map(params![memory_id], row_to_revision)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Get one revision by id.
    pub fn get_revision(&self, revision_id: i64) -> Result<Option<MemoryRevision>> {
        self.conn
            .query_row(
                "SELECT id, memory_id, title, memory_text, category, scope_kind, scope_id,
                        source, freshness, revision_source, created_at
                 FROM memory_revision WHERE id = ?1",
                params![revision_id],
                row_to_revision,
            )
            .optional()
            .map_err(Into::into)
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
    // Status + recall
    // -----------------------------------------------------------------------

    /// Aggregated store status for TUI/slash-command output.
    pub fn status(&self) -> Result<BrainStatus> {
        let settings = self.settings()?;
        let category_counts = self.category_counts()?;
        let page_count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM memory", [], |row| row.get(0))?;
        let relation_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM memory_relation", [], |row| row.get(0))?;
        let source_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM memory_source", [], |row| row.get(0))?;
        let revision_count: i64 =
            self.conn
                .query_row("SELECT COUNT(*) FROM memory_revision", [], |row| row.get(0))?;
        let global_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory WHERE scope_kind = 'global'",
            [],
            |row| row.get(0),
        )?;
        let workspace_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM memory WHERE scope_kind = 'workspace'",
            [],
            |row| row.get(0),
        )?;
        Ok(BrainStatus {
            settings,
            page_count: page_count as usize,
            relation_count: relation_count as usize,
            source_count: source_count as usize,
            category_counts,
            global_count: global_count as usize,
            workspace_count: workspace_count as usize,
            revision_count: revision_count as usize,
            procedure_count: self.procedure_count()? as usize,
            graph_regime: self
                .graph_health(crate::HealthThresholds::default())?
                .map(|health| health.regime()),
        })
    }

    /// Query-aware recall over global + active workspace pages.
    pub fn recall_pages(&self, options: RecallOptions) -> Result<Vec<RecalledMemoryPage>> {
        let limit = if options.limit == 0 {
            20
        } else {
            options.limit
        };
        let pages = self.list_pages_for_scope(options.workspace_scope.as_deref())?;
        let query_terms = tokenize_query(&options.query);
        let workspace = options
            .workspace_scope
            .and_then(|scope| normalize_scope_id(&scope));
        let mut scored = Vec::new();
        for (recency_rank, page) in pages.into_iter().enumerate() {
            let source_labels = self
                .sources(page.id)?
                .into_iter()
                .map(|source| source.label)
                .filter(|label| !label.trim().is_empty())
                .collect::<Vec<_>>();
            let mut score = 0i64;
            let title = page.title.to_ascii_lowercase();
            let body = page.memory_text.to_ascii_lowercase();
            let category = page.category.as_str();
            let mut matched_terms = 0i64;
            for term in &query_terms {
                let mut matched = false;
                if title.contains(term) {
                    score += 80;
                    matched = true;
                }
                if category.contains(term) {
                    score += 16;
                    matched = true;
                }
                if body.contains(term) {
                    score += 24;
                    matched = true;
                }
                if matched {
                    matched_terms += 1;
                }
            }
            if matched_terms > 1 {
                score += matched_terms * 20;
            }
            if let Some(scope) = workspace.as_deref()
                && page.scope_kind == MemoryScopeKind::Workspace
                && page.scope_id.as_deref() == Some(scope)
            {
                score += 60;
            }
            if page.freshness == MemoryFreshness::TimeSensitive
                && query_mentions_current_state(&query_terms)
            {
                score += 35;
            }
            if should_include_global_preference(&query_terms, &page) {
                score += 25;
            }
            // Recency is only a tie-breaker-scale signal. Durable facts should
            // not outrank a direct query match merely because they were updated
            // yesterday.
            score += (20 - recency_rank.min(20)) as i64;
            scored.push(RecalledMemoryPage {
                page,
                score,
                source_labels,
            });
        }
        sort_recalled_pages(&mut scored);
        let always_include = scored
            .iter()
            .filter(|recalled| should_include_global_preference(&query_terms, &recalled.page))
            .cloned()
            .collect::<Vec<_>>();
        if scored.len() > limit {
            scored.truncate(limit);
            for recalled in always_include {
                if scored
                    .iter()
                    .any(|selected| selected.page.id == recalled.page.id)
                {
                    continue;
                }
                if let Some(replace_idx) = scored.iter().rposition(|selected| {
                    !should_include_global_preference(&query_terms, &selected.page)
                }) {
                    scored[replace_idx] = recalled;
                }
            }
            sort_recalled_pages(&mut scored);
        }
        Ok(scored)
    }

    // -----------------------------------------------------------------------
    // Brain settings
    // -----------------------------------------------------------------------

    /// Whether the settings singleton has been persisted.
    pub fn settings_initialized(&self) -> Result<bool> {
        let exists: i64 = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM brain_settings WHERE id = 1)",
            [],
            |row| row.get(0),
        )?;
        Ok(exists != 0)
    }

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

fn freshness_for_source(source: Option<&str>) -> MemoryFreshness {
    match source {
        Some("current_state") | Some("git_state") | Some("sync_state") => {
            MemoryFreshness::TimeSensitive
        }
        _ => MemoryFreshness::Durable,
    }
}

/// Normalize an unordered pair to `(low, high)` — the storage invariant that
/// makes edge dedup fall out of the primary key (Onyx `_ordered_pair`).
fn ordered_pair(a: i64, b: i64) -> (i64, i64) {
    if a < b { (a, b) } else { (b, a) }
}

fn row_to_page(row: &Row<'_>) -> rusqlite::Result<MemoryPage> {
    let category_raw: String = row.get(3)?;
    let scope_raw: String = row.get(4)?;
    Ok(MemoryPage {
        id: row.get(0)?,
        title: row.get(1)?,
        memory_text: row.get(2)?,
        category: MemoryCategory::parse(&category_raw).unwrap_or(MemoryCategory::Notes),
        scope_kind: MemoryScopeKind::parse(&scope_raw),
        scope_id: row.get(5)?,
        source: row.get(6)?,
        freshness: MemoryFreshness::parse(&row.get::<_, String>(7)?),
        created_at: parse_ts(row.get::<_, String>(8)?),
        updated_at: parse_ts(row.get::<_, String>(9)?),
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

fn row_to_revision(row: &Row<'_>) -> rusqlite::Result<MemoryRevision> {
    let category_raw: String = row.get(4)?;
    let scope_raw: String = row.get(5)?;
    Ok(MemoryRevision {
        id: row.get(0)?,
        memory_id: row.get(1)?,
        title: row.get(2)?,
        memory_text: row.get(3)?,
        category: MemoryCategory::parse(&category_raw).unwrap_or(MemoryCategory::Notes),
        scope_kind: MemoryScopeKind::parse(&scope_raw),
        scope_id: row.get(6)?,
        source: row.get(7)?,
        freshness: MemoryFreshness::parse(&row.get::<_, String>(8)?),
        revision_source: row.get(9)?,
        created_at: parse_ts(row.get::<_, String>(10)?),
    })
}

fn sort_recalled_pages(pages: &mut [RecalledMemoryPage]) {
    pages.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.page.updated_at.cmp(&a.page.updated_at))
            .then_with(|| b.page.id.cmp(&a.page.id))
    });
}

/// Canonicalize a workspace scope, or `None` for "not workspace-scoped".
///
/// Strips trailing slashes BEFORE the emptiness check, not after. The previous
/// order mapped a root workspace `"/"` to `Some("")` — a workspace scope that
/// no checkout can ever match, so a page scoped to it was invisible to every
/// scoped query and to every unscoped one. `"/"` now reads as global, which is
/// the only reachable answer. Matches `procedure::normalize_procedure_scope`,
/// so the two scope columns cannot drift apart.
fn normalize_scope_id(scope_id: &str) -> Option<String> {
    let trimmed = scope_id.trim().trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn tokenize_query(query: &str) -> Vec<String> {
    query
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|term| term.len() >= 3)
        .take(24)
        .map(str::to_owned)
        .collect()
}

fn should_include_global_preference(query_terms: &[String], page: &MemoryPage) -> bool {
    if page.scope_kind != MemoryScopeKind::Global {
        return false;
    }
    let text = format!("{} {}", page.title, page.memory_text).to_ascii_lowercase();
    let is_preference = page.category == MemoryCategory::Notes
        || text.contains("user preference")
        || text.contains("the user prefers")
        || text.contains("shell")
        || text.contains("path preference");
    if !is_preference {
        return false;
    }
    query_terms.is_empty()
        || query_terms.iter().any(|term| {
            matches!(
                term.as_str(),
                "preference"
                    | "preferences"
                    | "path"
                    | "shell"
                    | "safety"
                    | "safe"
                    | "confirm"
                    | "confirmation"
                    | "account"
                    | "credentials"
                    | "token"
                    | "privacy"
                    | "sensitive"
            ) || text.contains(term)
        })
}

fn query_mentions_current_state(query_terms: &[String]) -> bool {
    query_terms.iter().any(|term| {
        matches!(
            term.as_str(),
            "current"
                | "state"
                | "branch"
                | "branches"
                | "remote"
                | "origin"
                | "upstream"
                | "deploy"
                | "deployment"
                | "sync"
                | "status"
                | "main"
                | "dev"
                | "pr"
        )
    })
}

/// Schema revision for the `memory_procedure` repair. Bump when the repair
/// itself changes and must run again on stores it has already visited.
const MEMORY_PROCEDURE_SCHEMA_VERSION: i64 = 1;

fn migrate_schema(conn: &Connection) -> Result<()> {
    add_column_if_missing(
        conn,
        "memory",
        "scope_kind",
        "TEXT NOT NULL DEFAULT 'global'",
    )?;
    add_column_if_missing(conn, "memory", "scope_id", "TEXT")?;
    add_column_if_missing(
        conn,
        "memory",
        "freshness",
        "TEXT NOT NULL DEFAULT 'durable'",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS memory_revision (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            memory_id INTEGER NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
            title TEXT NOT NULL,
            memory_text TEXT NOT NULL,
            category TEXT NOT NULL,
            scope_kind TEXT NOT NULL DEFAULT 'global',
            scope_id TEXT,
            source TEXT,
            freshness TEXT NOT NULL DEFAULT 'durable',
            revision_source TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ix_memory_scope ON memory(scope_kind, scope_id);
        CREATE INDEX IF NOT EXISTS ix_memory_revision_memory ON memory_revision(memory_id);

CREATE TABLE IF NOT EXISTS memory_procedure (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    signature TEXT NOT NULL,
    signature_key TEXT NOT NULL,
    plan TEXT NOT NULL,
    outcome TEXT NOT NULL,
    scope_id TEXT,
    uses INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_memory_procedure_key ON memory_procedure(signature_key);
CREATE INDEX IF NOT EXISTS ix_memory_procedure_outcome ON memory_procedure(outcome);",
    )?;

    add_column_if_missing(
        conn,
        "memory_revision",
        "freshness",
        "TEXT NOT NULL DEFAULT 'durable'",
    )?;

    // `failures` must exist BEFORE the repair below, which folds it. Ordering
    // this after the fold would silently drop duplicates' failure counts.
    add_column_if_missing(
        conn,
        "memory_procedure",
        "failures",
        "INTEGER NOT NULL DEFAULT 0",
    )?;

    // Repair, then collapse, then constrain. Creating the unique index over a
    // table that already violates it fails hard inside `migrate_schema`, and
    // `BrainService::open` goes through `BrainStore::open` — so one bad
    // procedure row would make the whole store permanently unopenable: pages,
    // search, graph and self-improvement too.
    //
    // One transaction, because the steps are not independently valid: a crash
    // between the fold and the delete would leave counters folded AND
    // duplicates present, and the next open would fold them a second time.
    // IMMEDIATE, not the default DEFERRED: the repair reads (SELECT DISTINCT)
    // before it writes, and a deferred read-then-write upgrade that loses the
    // race returns SQLITE_BUSY_SNAPSHOT WITHOUT invoking the busy handler — so
    // the timeout would not apply and `BrainStore::open` would simply fail when
    // two processes open the same store at once. Taken by hand because
    // `migrate_schema` holds `&Connection`, not `&mut`.
    // Gate on a schema version so the repair runs ONCE, not on every open.
    // Ungated it drops and rebuilds the identity index and runs two grouped
    // scans every time `BrainStore::open` is called — and open backs
    // memory_get, memory_search, the brain tools and the session hooks, so an
    // ungated write transaction on those read paths is a concurrency hazard,
    // not just wasted work.
    //
    // The gate checks the INVARIANT, not just the stamp. Guarding on the stamp
    // alone lets a store be marked migrated while the identity index is absent
    // — the table is created by the ungated schema batch but the index only by
    // this repair, so the two are otherwise guarded by different conditions —
    // and the index is then never restored. The extra check is one catalogue
    // lookup and makes the index self-healing.
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if applied < MEMORY_PROCEDURE_SCHEMA_VERSION || !identity_index_present(conn)? {
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let repaired = repair_memory_procedure(conn).and_then(|()| {
            // Stamped inside the transaction, so a rollback also un-stamps it
            // and the repair is retried rather than silently skipped.
            conn.execute_batch(&format!(
                "PRAGMA user_version = {MEMORY_PROCEDURE_SCHEMA_VERSION}"
            ))
            .map_err(Into::into)
        });
        if repaired.is_err() {
            // Leave no open transaction behind for the caller's next statement.
            let _ = conn.execute_batch("ROLLBACK");
        }
        repaired?;
        conn.execute_batch("COMMIT")?;
    }

    Ok(())
}

/// Whether `memory_procedure`'s identity index exists on the right table.
fn identity_index_present(conn: &Connection) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = ?1 AND tbl_name = 'memory_procedure'",
        params!["ux_memory_procedure_identity"],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// Canonicalize scopes, fold duplicates, and restore the identity index.
///
/// Split out so the caller can roll back as a unit; every statement here
/// assumes it runs inside a transaction.
fn repair_memory_procedure(tx: &Connection) -> Result<()> {
    // The index must go FIRST. `CREATE UNIQUE INDEX IF NOT EXISTS` is a no-op
    // on a store an earlier build already migrated, but the canonicalization
    // below is not — and collapsing two slash-spellings onto one key violates
    // an index that already exists, bricking the store on the upgrade path the
    // repair was written to protect. It is recreated at the end of this same
    // transaction, so no other connection ever observes the table unconstrained.
    // Qualified by table: SQLite index names are global to the database, so an
    // unqualified drop by name would silently remove an identically-named index
    // belonging to some other table.
    if identity_index_present(tx)? {
        tx.execute("DROP INDEX ux_memory_procedure_identity", [])?;
    }

    // Canonicalize scopes in RUST, not SQL. SQLite's one-argument `trim()`
    // strips U+0020 only, while `str::trim()` strips every Unicode whitespace
    // character, so an SQL rewrite leaves a scope like "/repo/a\n" — exactly
    // what an earlier unnormalized build would store from a captured path — in
    // a spelling `normalize_procedure_scope` can never produce, and therefore
    // in a row no query can ever reach. Sharing the one normalizer is the only
    // way the two cannot drift apart again.
    let stale: Vec<String> = {
        let mut stmt = tx
            .prepare("SELECT DISTINCT scope_id FROM memory_procedure WHERE scope_id IS NOT NULL")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for raw in stale {
        let canonical = crate::procedure::normalize_procedure_scope(Some(&raw));
        if canonical.as_deref() != Some(raw.as_str()) {
            tx.execute(
                "UPDATE memory_procedure SET scope_id = ?1 WHERE scope_id = ?2",
                params![canonical, raw],
            )?;
        }
    }

    // Fold every duplicate's counters into the lowest-id survivor. BOTH
    // counters, or the survivor's reliability is computed from successes it
    // kept and failures it lost.
    // The identity of a procedure row, written ONCE and reused by the fold, the
    // grouping, the delete and the index. Spelling it out per site is how the
    // generation-3 miss happened: the `failures` term was added to one copy of
    // the fold and not the other, so half the counters were silently dropped.
    const IDENTITY: &str = "signature_key, plan, COALESCE(scope_id, '')";
    // Duplicates OF the row being updated: same identity, higher id. Disjoint
    // from the updated set, which is why the fold cannot double-count.
    const DUP_OF_SURVIVOR: &str = "dup.signature_key = memory_procedure.signature_key
                   AND dup.plan = memory_procedure.plan
                   AND COALESCE(dup.scope_id, '') = COALESCE(memory_procedure.scope_id, '')
                   AND dup.id > memory_procedure.id";
    // Saturating, because SQLite promotes an i64 overflow to REAL instead of
    // erroring and the typed read then rejects the column outright.
    let fold = |column: &str| {
        format!(
            "{column} = MIN(9223372036854775807, {column} + COALESCE((
                 SELECT SUM(dup.{column}) FROM memory_procedure dup
                 WHERE {DUP_OF_SURVIVOR}), 0))"
        )
    };
    tx.execute_batch(&format!(
        "UPDATE memory_procedure SET
             {uses},
             {failures}
         WHERE id IN (
             SELECT MIN(id) FROM memory_procedure
             GROUP BY {IDENTITY}
             HAVING COUNT(*) > 1);
         DELETE FROM memory_procedure WHERE id NOT IN (
             SELECT MIN(id) FROM memory_procedure GROUP BY {IDENTITY});
         CREATE UNIQUE INDEX ux_memory_procedure_identity
             ON memory_procedure({IDENTITY});",
        uses = fold("uses"),
        failures = fold("failures"),
    ))?;
    Ok(())
}

fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .any(|name| name == column);
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))?;
    }
    Ok(())
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
    scope_kind TEXT NOT NULL DEFAULT 'global',
    scope_id TEXT,
    source TEXT,
    freshness TEXT NOT NULL DEFAULT 'durable',
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

CREATE TABLE IF NOT EXISTS memory_revision (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id INTEGER NOT NULL REFERENCES memory(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    memory_text TEXT NOT NULL,
    category TEXT NOT NULL,
    scope_kind TEXT NOT NULL DEFAULT 'global',
    scope_id TEXT,
    source TEXT,
    freshness TEXT NOT NULL DEFAULT 'durable',
    revision_source TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_memory_revision_memory ON memory_revision(memory_id);

CREATE TABLE IF NOT EXISTS brain_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    enabled INTEGER NOT NULL DEFAULT 0,
    use_connectors INTEGER NOT NULL DEFAULT 0,
    focus_instructions TEXT,
    last_run_at TEXT
);
";
