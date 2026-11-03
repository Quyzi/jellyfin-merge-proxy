//! Fan-out list specs: how each list endpoint is queried and merged.

// ---------------------------------------------------------------------------
// Fan-out lists
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    QueryResult,
    Array,
    SearchHints,
}

/// How a fan-out list endpoint is queried and merged.
pub struct ListSpec {
    /// Metrics label.
    pub name: &'static str,
    /// Canonical (modern) upstream path.
    pub path: Vec<String>,
    pub shape: Shape,
    /// Sort when the client sends no SortBy (fan-out only).
    pub default_sort: &'static [(&'static str, bool)],
    pub dedupe_items: bool,
    pub dedupe_by_name: bool,
    /// `Limit` assumed when the client sends none (Jellyfin's own default).
    pub default_limit: Option<usize>,
    /// Item field (case-insensitive value) of which a merged list keeps one
    /// item, e.g. `SeriesName` for NextUp.
    pub dedupe_key: Option<&'static str>,
}

macro_rules! spec {
    ($name:ident, $label:expr, [$($seg:expr),*], $shape:expr, $sort:expr, $dd:expr, $dn:expr, $limit:expr, $key:expr) => {
        pub static $name: std::sync::LazyLock<ListSpec> = std::sync::LazyLock::new(|| ListSpec {
            name: $label,
            path: vec![$($seg.to_string()),*],
            shape: $shape,
            default_sort: $sort,
            dedupe_items: $dd,
            dedupe_by_name: $dn,
            default_limit: $limit,
            dedupe_key: $key,
        });
    };
}
spec!(
    ITEMS,
    "items",
    ["Items"],
    Shape::QueryResult,
    &[],
    true,
    false,
    None,
    None
);
spec!(
    LATEST,
    "latest",
    ["Items", "Latest"],
    Shape::Array,
    &[("DateCreated", true)],
    true,
    false,
    Some(20),
    None
);
spec!(
    RESUME,
    "resume",
    ["UserItems", "Resume"],
    Shape::QueryResult,
    &[("DatePlayed", true)],
    false,
    false,
    None,
    None
);
spec!(
    NEXTUP,
    "nextup",
    ["Shows", "NextUp"],
    Shape::QueryResult,
    &[],
    false,
    false,
    None,
    Some("SeriesName")
);
spec!(
    SEARCH,
    "search",
    ["Search", "Hints"],
    Shape::SearchHints,
    &[],
    true,
    false,
    None,
    None
);
spec!(
    SEASONS,
    "seasons",
    ["Shows", "{parent}", "Seasons"],
    Shape::QueryResult,
    &[("IndexNumber", false)],
    false,
    false,
    None,
    None
);
spec!(
    EPISODES,
    "episodes",
    ["Shows", "{parent}", "Episodes"],
    Shape::QueryResult,
    &[("ParentIndexNumber", false), ("IndexNumber", false)],
    false,
    false,
    None,
    None
);
#[allow(non_upper_case_globals)]
pub const NAMED: ListSpec = ListSpec {
    name: "named",
    path: Vec::new(),
    shape: Shape::QueryResult,
    default_sort: &[("SortName", false)],
    dedupe_items: false,
    dedupe_by_name: true,
    default_limit: None,
    dedupe_key: None,
};

// Merged endpoints (see `merged.rs`): Items/Suggestions and the Live TV lists.
spec!(
    SUGGESTIONS,
    "suggestions",
    ["Items", "Suggestions"],
    Shape::QueryResult,
    &[("Random", false)],
    true,
    false,
    None,
    None
);
spec!(
    LIVE_CHANNELS,
    "livetv_channels",
    ["LiveTv", "Channels"],
    Shape::QueryResult,
    &[],
    false,
    false,
    None,
    None
);
spec!(
    LIVE_PROGRAMS,
    "livetv_programs",
    ["LiveTv", "Programs"],
    Shape::QueryResult,
    &[("StartDate", false)],
    false,
    false,
    None,
    None
);
spec!(
    LIVE_RECOMMENDED,
    "livetv_recommended",
    ["LiveTv", "Programs", "Recommended"],
    Shape::QueryResult,
    &[],
    false,
    false,
    None,
    None
);
spec!(
    LIVE_RECORDINGS,
    "livetv_recordings",
    ["LiveTv", "Recordings"],
    Shape::QueryResult,
    &[],
    false,
    false,
    None,
    None
);
spec!(
    LIVE_RECORDING_FOLDERS,
    "livetv_recording_folders",
    ["LiveTv", "Recordings", "Folders"],
    Shape::QueryResult,
    &[],
    false,
    false,
    None,
    None
);
spec!(
    LIVE_TIMERS,
    "livetv_timers",
    ["LiveTv", "Timers"],
    Shape::QueryResult,
    &[],
    false,
    false,
    None,
    None
);
spec!(
    LIVE_SERIES_TIMERS,
    "livetv_series_timers",
    ["LiveTv", "SeriesTimers"],
    Shape::QueryResult,
    &[],
    false,
    false,
    None,
    None
);
