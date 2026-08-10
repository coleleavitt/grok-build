//! Staleness that crosses relations.
//!
//! [`MemoryFreshness`] is a per-page flag: a page is either `Durable` or
//! `TimeSensitive`. Nothing propagates it. So a page marked `Durable` whose
//! neighbours are all operational state reads as durable with no caveat, even
//! though a conclusion assembled from time-sensitive inputs is itself only as
//! durable as those inputs.
//!
//! This is the hub-degradation reading from `tau_pathology_default_mode_network`
//! applied to a memory graph: a degraded hub does not announce itself, it
//! quietly poisons everything that depends on it. The Brain already knows which
//! pages are time-sensitive and which pages relate to which; it just never
//! joined the two.
//!
//! # Derived, never stored
//!
//! Like [`GraphHealth`](crate::GraphHealth), this is computed on read rather
//! than written to a column. A stored flag would need invalidating every time
//! any neighbour changed freshness, and a stale staleness marker is worse than
//! none. It also means the rule can change without a migration.
//!
//! # It refuses to guess
//!
//! One time-sensitive neighbour is not a pattern. Below
//! [`MIN_NEIGHBOURS_FOR_INHERITED_VERDICT`] the verdict is
//! [`Staleness::Own`] — the page's own flag and nothing more. Same principle as
//! returning `None` for an empty graph: a verdict the data cannot support is
//! worse than no verdict.
//!
//! # What it deliberately does not consider
//!
//! `memory_source` rows carry no freshness of their own, so citations are not
//! part of this. A page sourced entirely from a news article is not detectable
//! here, and claiming otherwise would overstate what the data supports.

use crate::{MemoryFreshness, Result};

/// Below this neighbour count no inherited verdict is issued. One
/// time-sensitive neighbour is a coincidence, not a signal.
pub const MIN_NEIGHBOURS_FOR_INHERITED_VERDICT: usize = 2;

/// Fraction of time-sensitive neighbours at or above which a durable page is
/// treated as inheriting their staleness.
///
/// A judgement call, like the graph-health thresholds. Half is the point at
/// which "most of what this page rests on can go out of date" becomes the
/// honest description.
pub const INHERITED_STALE_AT: f64 = 0.5;

/// What a page's freshness means once its neighbours are taken into account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Staleness {
    /// The page's own flag stands: either it is time-sensitive itself, or it
    /// has too few neighbours for an inherited verdict to mean anything.
    Own(MemoryFreshness),
    /// Marked durable, but most of what it relates to is time-sensitive. The
    /// conclusion is only as durable as the inputs it rests on.
    InheritedSuspect,
}

impl Staleness {
    /// Whether the page should be verified before being relied on — true for a
    /// page that is time-sensitive itself OR inherits staleness.
    pub fn needs_verification(self) -> bool {
        !matches!(self, Self::Own(MemoryFreshness::Durable))
    }
}

/// A page's freshness together with the neighbour evidence behind the verdict.
///
/// The counts are public because the verdict is a judgement call over them: a
/// caller that disagrees with [`INHERITED_STALE_AT`] can re-decide from the
/// same numbers rather than trusting the enum.
#[derive(Debug, Clone, PartialEq)]
pub struct PageStaleness {
    pub page_id: i64,
    /// The page's own stored flag.
    pub own: MemoryFreshness,
    /// How many pages this one relates to.
    pub neighbours: usize,
    /// How many of those are time-sensitive.
    pub time_sensitive_neighbours: usize,
    pub verdict: Staleness,
}

impl PageStaleness {
    /// Fraction of neighbours that are time-sensitive, or `None` when the page
    /// has no relations at all — distinct from 0.0, which means it has
    /// neighbours and none of them are stale.
    pub fn time_sensitive_fraction(&self) -> Option<f64> {
        (self.neighbours > 0)
            .then(|| self.time_sensitive_neighbours as f64 / self.neighbours as f64)
    }
}

impl crate::BrainStore {
    /// Freshness for one page, accounting for what it relates to.
    ///
    /// `None` when the page does not exist.
    pub fn page_staleness(&self, id: i64) -> Result<Option<PageStaleness>> {
        let Some(page) = self.get_page(id)? else {
            return Ok(None);
        };
        let neighbour_ids = self.related_page_ids(id)?;
        let mut time_sensitive = 0usize;
        for neighbour in &neighbour_ids {
            if let Some(neighbour) = self.get_page(*neighbour)?
                && neighbour.freshness == MemoryFreshness::TimeSensitive
            {
                time_sensitive += 1;
            }
        }

        let neighbours = neighbour_ids.len();
        // A page that is already time-sensitive cannot inherit anything it does
        // not already say, so its own flag stands and the neighbour scan is
        // only reported, never applied.
        let inherits = page.freshness == MemoryFreshness::Durable
            && neighbours >= MIN_NEIGHBOURS_FOR_INHERITED_VERDICT
            && (time_sensitive as f64 / neighbours as f64) >= INHERITED_STALE_AT;

        Ok(Some(PageStaleness {
            page_id: id,
            own: page.freshness,
            neighbours,
            time_sensitive_neighbours: time_sensitive,
            verdict: if inherits {
                Staleness::InheritedSuspect
            } else {
                Staleness::Own(page.freshness)
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BrainStore, MemoryCategory, NewPage, PageUpdate};

    fn store() -> BrainStore {
        BrainStore::open_in_memory().expect("open in-memory brain")
    }

    fn page(store: &BrainStore, title: &str, freshness: MemoryFreshness) -> i64 {
        let page = store
            .create_page(NewPage {
                title: Some(title.to_owned()),
                memory_text: format!("body of {title}"),
                category: MemoryCategory::Notes,
                source: None,
            })
            .expect("create page");
        if freshness != MemoryFreshness::Durable {
            store
                .update_page(
                    page.id,
                    PageUpdate {
                        freshness: Some(freshness),
                        ..PageUpdate::default()
                    },
                )
                .expect("set freshness");
        }
        page.id
    }

    /// The defect: a page marked durable whose neighbours are all operational
    /// state read as durable with no caveat.
    #[test]
    fn a_durable_page_resting_on_time_sensitive_neighbours_is_suspect() {
        let store = store();
        let conclusion = page(&store, "conclusion", MemoryFreshness::Durable);
        for i in 0..3 {
            let input = page(
                &store,
                &format!("input {i}"),
                MemoryFreshness::TimeSensitive,
            );
            store.add_relation(conclusion, input).unwrap();
        }

        let staleness = store.page_staleness(conclusion).unwrap().unwrap();
        assert_eq!(
            staleness.own,
            MemoryFreshness::Durable,
            "its own flag is unchanged"
        );
        assert_eq!(staleness.neighbours, 3);
        assert_eq!(staleness.time_sensitive_neighbours, 3);
        assert_eq!(staleness.verdict, Staleness::InheritedSuspect);
        assert!(
            staleness.verdict.needs_verification(),
            "a conclusion is only as durable as what it rests on",
        );
    }

    /// A durable page among durable neighbours must stay clean, or the signal
    /// is noise.
    #[test]
    fn a_durable_page_among_durable_neighbours_stays_durable() {
        let store = store();
        let page_id = page(&store, "settled", MemoryFreshness::Durable);
        for i in 0..3 {
            let neighbour = page(
                &store,
                &format!("also settled {i}"),
                MemoryFreshness::Durable,
            );
            store.add_relation(page_id, neighbour).unwrap();
        }

        let staleness = store.page_staleness(page_id).unwrap().unwrap();
        assert_eq!(staleness.time_sensitive_neighbours, 0);
        assert_eq!(staleness.verdict, Staleness::Own(MemoryFreshness::Durable));
        assert!(!staleness.verdict.needs_verification());
        assert_eq!(staleness.time_sensitive_fraction(), Some(0.0));
    }

    /// One time-sensitive neighbour is a coincidence. Issuing a verdict on it
    /// would make the signal fire constantly and stop meaning anything.
    #[test]
    fn a_single_time_sensitive_neighbour_does_not_trigger_a_verdict() {
        let store = store();
        let page_id = page(&store, "conclusion", MemoryFreshness::Durable);
        let input = page(&store, "one input", MemoryFreshness::TimeSensitive);
        store.add_relation(page_id, input).unwrap();

        let staleness = store.page_staleness(page_id).unwrap().unwrap();
        assert_eq!(staleness.neighbours, 1);
        assert_eq!(staleness.time_sensitive_neighbours, 1);
        assert_eq!(
            staleness.verdict,
            Staleness::Own(MemoryFreshness::Durable),
            "below the neighbour floor the page's own flag stands",
        );
        // The evidence is still reported, so a caller may decide otherwise.
        assert_eq!(staleness.time_sensitive_fraction(), Some(1.0));
    }

    /// A page with no relations has nothing to inherit, and that is distinct
    /// from having neighbours that are all fresh.
    #[test]
    fn an_unrelated_page_reports_no_fraction_rather_than_zero() {
        let store = store();
        let page_id = page(&store, "orphan", MemoryFreshness::Durable);
        let staleness = store.page_staleness(page_id).unwrap().unwrap();
        assert_eq!(staleness.neighbours, 0);
        assert_eq!(staleness.time_sensitive_fraction(), None);
        assert_eq!(staleness.verdict, Staleness::Own(MemoryFreshness::Durable));
    }

    /// A page that is already time-sensitive cannot inherit anything it does
    /// not already say; its own flag stands and still demands verification.
    #[test]
    fn an_already_time_sensitive_page_keeps_its_own_verdict() {
        let store = store();
        let page_id = page(&store, "operational", MemoryFreshness::TimeSensitive);
        for i in 0..3 {
            let neighbour = page(
                &store,
                &format!("input {i}"),
                MemoryFreshness::TimeSensitive,
            );
            store.add_relation(page_id, neighbour).unwrap();
        }

        let staleness = store.page_staleness(page_id).unwrap().unwrap();
        assert_eq!(
            staleness.verdict,
            Staleness::Own(MemoryFreshness::TimeSensitive),
            "it was already saying this; InheritedSuspect would add nothing",
        );
        assert!(staleness.verdict.needs_verification());
    }

    /// Exactly at the threshold the verdict fires, and just below it does not.
    #[test]
    fn the_inherited_threshold_is_pinned_from_both_sides() {
        let store = store();

        // 2 of 4 time-sensitive = exactly 0.5.
        let at = page(&store, "at", MemoryFreshness::Durable);
        for i in 0..2 {
            let n = page(
                &store,
                &format!("at stale {i}"),
                MemoryFreshness::TimeSensitive,
            );
            store.add_relation(at, n).unwrap();
        }
        for i in 0..2 {
            let n = page(&store, &format!("at fresh {i}"), MemoryFreshness::Durable);
            store.add_relation(at, n).unwrap();
        }
        assert_eq!(
            store.page_staleness(at).unwrap().unwrap().verdict,
            Staleness::InheritedSuspect,
            "at the threshold the verdict fires",
        );

        // 1 of 3 = 0.333, below it.
        let below = page(&store, "below", MemoryFreshness::Durable);
        let stale = page(&store, "below stale", MemoryFreshness::TimeSensitive);
        store.add_relation(below, stale).unwrap();
        for i in 0..2 {
            let n = page(
                &store,
                &format!("below fresh {i}"),
                MemoryFreshness::Durable,
            );
            store.add_relation(below, n).unwrap();
        }
        assert_eq!(
            store.page_staleness(below).unwrap().unwrap().verdict,
            Staleness::Own(MemoryFreshness::Durable),
        );
    }

    #[test]
    fn a_missing_page_has_no_staleness() {
        let store = store();
        assert!(store.page_staleness(9999).unwrap().is_none());
    }

    /// The verdict crosses the `brain_get` wire, so its spelling is a contract
    /// and is pinned in the crate that owns it.
    #[test]
    fn staleness_wire_spelling_is_pinned() {
        assert_eq!(
            serde_json::to_value(Staleness::InheritedSuspect).unwrap(),
            serde_json::json!("inherited_suspect"),
        );
        assert_eq!(
            serde_json::to_value(Staleness::Own(MemoryFreshness::TimeSensitive)).unwrap(),
            serde_json::json!({"own": "time_sensitive"}),
        );
    }
}
