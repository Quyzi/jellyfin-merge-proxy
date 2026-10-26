//! Translation between legacy (`/Users/{id}/...`, Jellyfin < 10.9) and
//! modern (`/UserItems/...`, `/Items?userId=`) route shapes.

fn eq(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

fn set_user(query: &mut Vec<(String, String)>, uid: &str) {
    query.retain(|(k, _)| !eq(k, "userId"));
    query.push(("userId".into(), uid.to_string()));
}

/// Adapt a path (already carrying upstream ids) to the upstream's version.
pub fn adapt(
    segs: Vec<String>,
    query: &mut Vec<(String, String)>,
    modern: bool,
    uid: Option<&str>,
) -> Vec<String> {
    let s: Vec<&str> = segs.iter().map(|s| s.as_str()).collect();
    let n = s.len();
    let owned = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    if modern {
        if n >= 3 && eq(s[0], "users") {
            let u = s[1].to_string();
            let out: Option<Vec<String>> = match (n, s[2].to_ascii_lowercase().as_str()) {
                (3, "items") => Some(owned(&["Items"])),
                (3, "views") => Some(owned(&["UserViews"])),
                (4, "items") if eq(s[3], "resume") => Some(owned(&["UserItems", "Resume"])),
                (4, "items") => Some(owned(&["Items", s[3]])),
                (5, "items") if eq(s[4], "userdata") || eq(s[4], "rating") => {
                    Some(owned(&["UserItems", s[3], s[4]]))
                }
                (5, "items") => Some(owned(&["Items", s[3], s[4]])),
                (4, "playeditems") => Some(owned(&["UserPlayedItems", s[3]])),
                (4, "favoriteitems") => Some(owned(&["UserFavoriteItems", s[3]])),
                (_, "playingitems") => {
                    let mut v = vec!["PlayingItems".to_string()];
                    v.extend(s[3..].iter().map(|x| x.to_string()));
                    Some(v)
                }
                _ => None,
            };
            if let Some(o) = out {
                set_user(query, &u);
                return o;
            }
        }
        if let Some(u) = uid {
            let first = s
                .first()
                .map(|x| x.to_ascii_lowercase())
                .unwrap_or_default();
            if matches!(
                first.as_str(),
                "items"
                    | "useritems"
                    | "userviews"
                    | "userplayeditems"
                    | "userfavoriteitems"
                    | "playingitems"
            ) || query.iter().any(|(k, _)| eq(k, "userId"))
            {
                set_user(query, u);
            }
        }
        return segs;
    }
    // Legacy upstream: turn modern shapes into /Users/{uid}/...
    let Some(u) = uid else { return segs };
    let l: Vec<String> = s.iter().map(|x| x.to_ascii_lowercase()).collect();
    let lv: Vec<&str> = l.iter().map(|x| x.as_str()).collect();
    let out: Option<Vec<String>> = match lv.as_slice() {
        ["useritems", "resume"] => Some(owned(&["Users", u, "Items", "Resume"])),
        ["useritems", _, x] if *x == "userdata" || *x == "rating" => {
            Some(owned(&["Users", u, "Items", s[1], s[2]]))
        }
        ["userplayeditems", _] => Some(owned(&["Users", u, "PlayedItems", s[1]])),
        ["userfavoriteitems", _] => Some(owned(&["Users", u, "FavoriteItems", s[1]])),
        ["userviews"] => Some(owned(&["Users", u, "Views"])),
        ["items", "latest"] => Some(owned(&["Users", u, "Items", "Latest"])),
        ["items", "root"] => Some(owned(&["Users", u, "Items", "Root"])),
        ["items"] => Some(owned(&["Users", u, "Items"])),
        ["items", id] if crate::ids::looks_like_id(id) => Some(owned(&["Users", u, "Items", s[1]])),
        ["items", id, x]
            if crate::ids::looks_like_id(id)
                && matches!(*x, "intros" | "localtrailers" | "specialfeatures") =>
        {
            Some(owned(&["Users", u, "Items", s[1], s[2]]))
        }
        ["playingitems", ..] => {
            let mut v = owned(&["Users", u, "PlayingItems"]);
            v.extend(s[1..].iter().map(|x| x.to_string()));
            Some(v)
        }
        _ => None,
    };
    if let Some(o) = out {
        if !query.iter().any(|(k, _)| eq(k, "userId")) {
            set_user(query, u);
        }
        return o;
    }
    // Make sure userId points at the upstream user where present.
    for (k, v) in query.iter_mut() {
        if eq(k, "userId") {
            *v = u.to_string();
        }
    }
    segs
}

#[cfg(test)]
mod tests {
    use super::*;
    fn v(s: &str) -> Vec<String> {
        s.split('/').map(String::from).collect()
    }
    #[test]
    fn legacy_to_modern() {
        let mut q = vec![];
        let out = adapt(v("Users/U1/Items/abc/UserData"), &mut q, true, Some("U1"));
        assert_eq!(out, v("UserItems/abc/UserData"));
        assert_eq!(q, vec![("userId".to_string(), "U1".to_string())]);
        let mut q = vec![];
        assert_eq!(
            adapt(v("Users/U1/PlayedItems/abc"), &mut q, true, Some("U1")),
            v("UserPlayedItems/abc")
        );
    }
    #[test]
    fn modern_to_legacy() {
        let mut q = vec![];
        assert_eq!(
            adapt(v("UserItems/Resume"), &mut q, false, Some("U")),
            v("Users/U/Items/Resume")
        );
        let mut q = vec![];
        let id = "0123456789abcdef0123456789abcdef";
        assert_eq!(
            adapt(vec!["Items".into(), id.into()], &mut q, false, Some("U")),
            vec!["Users".to_string(), "U".into(), "Items".into(), id.into()]
        );
        let mut q = vec![];
        assert_eq!(
            adapt(v("Videos/x/stream"), &mut q, false, Some("U")),
            v("Videos/x/stream")
        );
    }
}
