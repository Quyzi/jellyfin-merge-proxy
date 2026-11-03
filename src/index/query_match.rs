//! The per-item filter of [`IndexQuery`]: does one canonical item satisfy
//! every parameter of the query?

use super::*;

/// `PremiereDate` is in the future (`IsUnaired`); items without a parseable
/// premiere date are neither aired nor unaired.
fn unaired(premiere: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Option<bool> {
    let p = chrono::DateTime::parse_from_rfc3339(premiere?).ok()?;
    Some(p >= now)
}

impl IndexQuery {
    pub(super) fn type_ok(&self, kind: &str, list: &[String]) -> bool {
        list.iter().any(|t| t.eq_ignore_ascii_case(kind))
    }

    pub(super) fn matches(
        &self,
        snap: &Snapshot,
        idx: usize,
        user: &dyn Fn(&Item) -> Option<UserFact>,
    ) -> bool {
        if self.impossible {
            return false;
        }
        let it = &snap.items[idx];
        let e = snap.primary(it);
        match &self.parent {
            None => {}
            Some(Scope::Nothing) => return false,
            Some(Scope::Libraries(libs)) => {
                if !it.libraries.iter().any(|l| libs.contains(l)) {
                    return false;
                }
            }
            Some(Scope::Item(p)) => {
                let child = it.season == Some(*p)
                    || (it.series == Some(*p) && (self.recursive || e.is("Season")));
                if !child {
                    return false;
                }
            }
        }
        if !self.include_types.is_empty() && !self.type_ok(&e.kind, &self.include_types) {
            return false;
        }
        if self.type_ok(&e.kind, &self.exclude_types) {
            return false;
        }
        if !self.media_types.is_empty()
            && !e
                .media_type
                .as_deref()
                .is_some_and(|m| self.type_ok(m, &self.media_types))
        {
            return false;
        }
        let any = |want: &Option<Vec<usize>>, have: &[usize]| {
            want.as_ref()
                .is_none_or(|w| w.iter().any(|g| have.contains(g)))
        };
        if !any(&self.genres, &it.genres) || !any(&self.studios, &it.studios) {
            return false;
        }
        if let Some(w) = &self.persons {
            let hit = it.people.iter().any(|(n, role)| {
                w.contains(n)
                    && (self.person_types.is_empty() || self.type_ok(role, &self.person_types))
            });
            if !hit {
                return false;
            }
        }
        if !self.years.is_empty() && !e.year.is_some_and(|y| self.years.contains(&y)) {
            return false;
        }
        if !self.official.is_empty()
            && !e
                .official
                .as_deref()
                .is_some_and(|r| self.type_ok(r, &self.official))
        {
            return false;
        }
        if !self.tags.is_empty() || !self.exclude_tags.is_empty() {
            let have: Vec<String> = e.tags.iter().map(|t| clean(t)).collect();
            if !self.tags.is_empty() && !have.iter().any(|t| self.tags.contains(t)) {
                return false;
            }
            if have.iter().any(|t| self.exclude_tags.contains(t)) {
                return false;
            }
        }
        // `BaseItemRepository`: NameStartsWith is a prefix match on SortName.
        let sn = e.sort_name.as_str();
        if self
            .name_starts_with
            .as_ref()
            .is_some_and(|p| !sn.starts_with(p.as_str()))
        {
            return false;
        }
        if self
            .name_starts_with_or_greater
            .as_ref()
            .is_some_and(|p| sn < p.as_str())
            || self
                .name_less_than
                .as_ref()
                .is_some_and(|p| sn >= p.as_str())
        {
            return false;
        }
        if let Some(s) = &self.search {
            let by_title = self.search_original.as_ref().is_some_and(|o| {
                e.original_title
                    .as_ref()
                    .is_some_and(|t| t.to_lowercase().contains(o.as_str()))
            });
            if !clean(&e.name).contains(s.as_str()) && !by_title {
                return false;
            }
        }
        if let Some(v) = self.virtual_item.or(self.missing) {
            if snap.is_virtual(idx) != v {
                return false;
            }
        }
        if let Some(want) = self.unaired {
            if unaired(e.premiere.as_deref(), chrono::Utc::now()) != Some(want) {
                return false;
            }
        }
        if self.ids.as_ref().is_some_and(|ids| !ids.contains(&idx))
            || self.is_folder.is_some_and(|f| f != e.is_folder)
            || self
                .min_community
                .is_some_and(|m| e.community.is_none_or(|c| c < m))
            || self
                .min_critic
                .is_some_and(|m| e.critic.is_none_or(|c| c < m))
            || self.series.is_some_and(|s| it.series != Some(s))
            || self.season.is_some_and(|s| it.season != Some(s))
            || self
                .season_number
                .is_some_and(|n| e.parent_index != Some(n))
        {
            return false;
        }
        let needs_user = self.played.is_some()
            || self.favorite.is_some()
            || self.resumable
            || self.likes.is_some()
            || self.favorite_or_likes;
        if needs_user {
            let u = user(it).unwrap_or_default();
            if self.played.is_some_and(|p| p != u.played)
                || self.favorite.is_some_and(|f| f != u.favorite)
                || (self.resumable && u.position_ticks <= 0)
                || self.likes.is_some_and(|l| u.likes != Some(l))
                || (self.favorite_or_likes && !(u.favorite || u.likes == Some(true)))
            {
                return false;
            }
        }
        true
    }
}
