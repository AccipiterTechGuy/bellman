//! Resolve a timer by name or id against the store.

use bellman_core::store::{Store, StoreError, Timer, TimerId};
use uuid::Uuid;

/// Look up a timer by UUID string or exact name match.
///
/// Name match is case-sensitive and must be unique. Ambiguous names error.
pub fn resolve_timer(store: &Store, name_or_id: &str) -> Result<Timer, ResolveError> {
    if let Ok(id) = Uuid::parse_str(name_or_id) {
        return match store.get_timer(id) {
            Ok(Some(t)) => Ok(t),
            Ok(None) => Err(ResolveError::NotFound(name_or_id.to_string())),
            Err(e) => Err(ResolveError::Store(e)),
        };
    }

    let timers = store.list_timers().map_err(ResolveError::Store)?;
    let matches: Vec<_> = timers
        .into_iter()
        .filter(|t| t.name == name_or_id)
        .collect();
    match matches.len() {
        0 => Err(ResolveError::NotFound(name_or_id.to_string())),
        1 => Ok(matches.into_iter().next().expect("len 1")),
        n => Err(ResolveError::Ambiguous {
            name: name_or_id.to_string(),
            count: n,
            ids: matches.iter().map(|t| t.id).collect(),
        }),
    }
}

#[derive(Debug)]
pub enum ResolveError {
    NotFound(String),
    Ambiguous {
        name: String,
        count: usize,
        ids: Vec<TimerId>,
    },
    Store(StoreError),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(s) => write!(f, "timer not found: {s}"),
            Self::Ambiguous { name, count, ids } => {
                let id_list: Vec<String> = ids.iter().map(std::string::ToString::to_string).collect();
                write!(
                    f,
                    "ambiguous timer name '{name}' matches {count} timers ({}); use id",
                    id_list.join(", ")
                )
            }
            Self::Store(e) => write!(f, "{e}"),
        }
    }
}

impl ResolveError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Ambiguous { .. } => "ambiguous_name",
            Self::NotFound(_) | Self::Store(StoreError::NotFound(_)) => "not_found",
            Self::Store(StoreError::StaleRevision { .. }) => "stale_revision",
            Self::Store(StoreError::InvalidOccurrence(_)) => "invalid_occurrence",
            Self::Store(StoreError::NetworkFilesystem(_)) => "network_filesystem",
            Self::Store(_) => "store_error",
        }
    }
}
