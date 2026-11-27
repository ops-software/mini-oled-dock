//! The headless listing (SPEC-files 5.5.1-5.5.4, 5.5.11): entries in a WTF-8 arena, Explorer's natural order, the
//! enumerator with generations and its pool, the store with its sorts, filter and diffs. Every frozen item is
//! re-exported here, so callers may name either `listing::EntryStore` or `listing::store::EntryStore`.

pub use oled_trust::frame::files::{Gen, WidePath};

pub mod arena;
pub mod collate;
pub mod diff;
pub mod enum_worker;
pub mod filter;
pub mod natural;
pub mod pool;
pub mod sort;
pub mod store;
pub mod types;

pub use collate::{Collator, LogicalCollator, SortKeyCollator};
pub use diff::Diff;
pub use enum_worker::{stat_one, Enumerator, FindEnumerator, OwnedEntry};
pub use filter::FilterQuery;
pub use natural::{name_key, natural_cmp, natural_cmp_keyed, self_check, NameKey};
pub use pool::{is_network, BatchSink, EnumJob, EnumPool, MakeEnumerator, PoolStats};
pub use sort::{GroupBy, SortDir, SortDone, SortJob, SortKey};
pub use store::{rescan_merge, Anchor, ApplyStats, EntryStore, HeapParts, IdMap, Permutation, Visibility};
pub use types::{Batch, CancelToken, Entry, EntryId, EnumError, RawEntry, TabId};
