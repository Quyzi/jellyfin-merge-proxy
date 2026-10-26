#[derive(Clone, Copy, PartialEq)]
pub enum Shape {
    QueryResult,
    Array,
    SearchHints,
}

pub struct ListSpec {
    pub name: &'static str,
    pub path: Vec<String>,
    pub shape: Shape,
    pub default_sort: &'static [(&'static str, bool)],
    pub dedupe_items: bool,
    pub dedupe_by_name: bool,
}

macro_rules! spec {
    ($name:ident, $label:expr, [$($seg:expr),*], $shape:expr, $sort:expr, $dd:expr, $dn:expr) => {
        pub static $name: std::sync::LazyLock<ListSpec> = std::sync::LazyLock::new(|| ListSpec {
            name: $label,
            path: vec![$($seg.to_string()),*],
            shape: $shape,
            default_sort: $sort,
            dedupe_items: $dd,
            dedupe_by_name: $dn,
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
    false
);
spec!(
    LATEST,
    "latest",
    ["Items", "Latest"],
    Shape::Array,
    &[("DateCreated", true)],
    true,
    false
);
spec!(
    RESUME,
    "resume",
    ["UserItems", "Resume"],
    Shape::QueryResult,
    &[("DatePlayed", true)],
    false,
    false
);
spec!(
    NEXTUP,
    "nextup",
    ["Shows", "NextUp"],
    Shape::QueryResult,
    &[],
    false,
    false
);
spec!(
    SEARCH,
    "search",
    ["Search", "Hints"],
    Shape::SearchHints,
    &[],
    true,
    false
);
spec!(
    SEASONS,
    "seasons",
    ["Shows", "{parent}", "Seasons"],
    Shape::QueryResult,
    &[("IndexNumber", false)],
    false,
    false
);
spec!(
    EPISODES,
    "episodes",
    ["Shows", "{parent}", "Episodes"],
    Shape::QueryResult,
    &[("ParentIndexNumber", false), ("IndexNumber", false)],
    false,
    false
);

#[allow(non_upper_case_globals)]
pub const NAMED: ListSpec = ListSpec {
    name: "named",
    path: Vec::new(),
    shape: Shape::QueryResult,
    default_sort: &[("SortName", false)],
    dedupe_items: false,
    dedupe_by_name: true,
};
