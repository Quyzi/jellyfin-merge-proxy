//! Sorting and paging for merged item lists.

use serde_json::Value;
use std::cmp::Ordering;

pub type SortSpec = Vec<(String, bool)>;

pub fn parse_sort(sort_by: Option<&str>, sort_order: Option<&str>) -> SortSpec {
    let Some(sb) = sort_by.filter(|s| !s.trim().is_empty()) else {
        return vec![];
    };
    let orders: Vec<bool> = sort_order
        .unwrap_or("")
        .split(',')
        .map(|o| o.trim().eq_ignore_ascii_case("descending"))
        .collect();
    sb.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .enumerate()
        .map(|(i, f)| {
            let desc = orders
                .get(i)
                .copied()
                .unwrap_or_else(|| orders.last().copied().unwrap_or(false));
            (f, desc)
        })
        .collect()
}

fn get_path<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for p in path {
        cur = cur.get(*p)?;
    }
    if cur.is_null() {
        None
    } else {
        Some(cur)
    }
}

fn sort_value<'a>(item: &'a Value, field: &str) -> Option<&'a Value> {
    let f = field.to_ascii_lowercase();
    match f.as_str() {
        "sortname" => get_path(item, &["SortName"]).or_else(|| get_path(item, &["Name"])),
        "seriessortname" => get_path(item, &["SeriesName"]),
        "runtime" => get_path(item, &["RunTimeTicks"]),
        "dateplayed" => get_path(item, &["UserData", "LastPlayedDate"]),
        "playcount" => get_path(item, &["UserData", "PlayCount"]),
        "isplayed" => get_path(item, &["UserData", "Played"]),
        "isfavoriteorliked" => get_path(item, &["UserData", "IsFavorite"]),
        "artist" => get_path(item, &["Artists", "0"]).or_else(|| {
            item.get("Artists")
                .and_then(|a| a.get(0))
                .filter(|v| !v.is_null())
        }),
        "datelastcontentadded" => {
            get_path(item, &["DateLastMediaAdded"]).or_else(|| get_path(item, &["DateCreated"]))
        }
        _ => item.as_object().and_then(|o| {
            o.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(field))
                .map(|(_, v)| v)
                .filter(|v| !v.is_null())
        }),
    }
}

fn cmp_values(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&y.as_f64().unwrap_or(0.0))
            .unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => natural_cmp(&x.to_lowercase(), &y.to_lowercase()),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    bi.next();
                }
                let na = na.trim_start_matches('0');
                let nb = nb.trim_start_matches('0');
                let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

pub fn sort_items(items: &mut [Value], spec: &SortSpec) {
    if spec.is_empty() {
        return;
    }
    if spec.iter().any(|(f, _)| f.eq_ignore_ascii_case("random")) {
        use rand::seq::SliceRandom;
        items.shuffle(&mut rand::thread_rng());
        return;
    }
    items.sort_by(|a, b| {
        for (field, desc) in spec {
            let o = match (sort_value(a, field), sort_value(b, field)) {
                (Some(x), Some(y)) => {
                    let o = cmp_values(x, y);
                    if *desc {
                        o.reverse()
                    } else {
                        o
                    }
                }
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            };
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
}

pub fn page(items: Vec<Value>, start: usize, limit: Option<usize>) -> Vec<Value> {
    let it = items.into_iter().skip(start);
    match limit {
        Some(l) => it.take(l).collect(),
        None => it.collect(),
    }
}
