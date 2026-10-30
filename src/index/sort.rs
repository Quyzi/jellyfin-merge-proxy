//! Jellyfin sort semantics over the index.
//!
//! Keys are compared like Jellyfin's database does: values come from the
//! index (so keys absent from a DTO sort the same as on a real server),
//! missing values sort first ascending and last descending (SQL NULL
//! ordering), and parental ratings sort by the server's rating score. Ties
//! break on SortName and then id, so deep paging is stable.

use super::query::UserFact;
use super::snapshot::{Item, Snapshot};
use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    SortName,
    Name,
    DateCreated,
    PremiereDate,
    ProductionYear,
    CommunityRating,
    CriticRating,
    OfficialRating,
    Runtime,
    Random,
    IsFolder,
    SeriesSortName,
    ParentIndexNumber,
    IndexNumber,
    AiredEpisodeOrder,
    DateLastContentAdded,
    // Per-user (need user data).
    PlayCount,
    DatePlayed,
    IsPlayed,
    IsUnplayed,
    IsFavoriteOrLiked,
    SeriesDatePlayed,
}

impl SortKey {
    pub fn parse(s: &str) -> Option<SortKey> {
        use SortKey::*;
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "sortname" | "default" => SortName,
            "name" => Name,
            "datecreated" => DateCreated,
            "premieredate" => PremiereDate,
            "productionyear" => ProductionYear,
            "communityrating" => CommunityRating,
            "criticrating" => CriticRating,
            "officialrating" => OfficialRating,
            "runtime" => Runtime,
            "random" => Random,
            "isfolder" => IsFolder,
            "seriessortname" => SeriesSortName,
            "parentindexnumber" => ParentIndexNumber,
            "indexnumber" => IndexNumber,
            "airedepisodeorder" => AiredEpisodeOrder,
            "datelastcontentadded" => DateLastContentAdded,
            "playcount" => PlayCount,
            "dateplayed" => DatePlayed,
            "isplayed" => IsPlayed,
            "isunplayed" => IsUnplayed,
            "isfavoriteorliked" => IsFavoriteOrLiked,
            "seriesdateplayed" => SeriesDatePlayed,
            _ => return None,
        })
    }

    pub fn needs_user(self) -> bool {
        use SortKey::*;
        matches!(
            self,
            PlayCount | DatePlayed | IsPlayed | IsUnplayed | IsFavoriteOrLiked | SeriesDatePlayed
        )
    }
}

/// A sortable value; `None` = SQL NULL.
#[derive(Debug, Clone, PartialEq, PartialOrd)]
enum V {
    I(i64),
    F(f64),
    S(String),
}

fn cmp_opt(a: &Option<V>, b: &Option<V>) -> Ordering {
    match (a, b) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
    }
}

/// Everything a comparison may need besides the items.
pub struct SortCtx<'a> {
    pub snap: &'a Snapshot,
    pub user: &'a dyn Fn(&Item) -> Option<UserFact>,
    /// Latest DateCreated below each series (DateLastContentAdded).
    last_added: HashMap<usize, String>,
    random: HashMap<usize, u64>,
}

impl<'a> SortCtx<'a> {
    pub fn new(
        snap: &'a Snapshot,
        user: &'a dyn Fn(&Item) -> Option<UserFact>,
        keys: &[(SortKey, bool)],
        candidates: &[usize],
    ) -> Self {
        let mut last_added = HashMap::new();
        if keys
            .iter()
            .any(|(k, _)| *k == SortKey::DateLastContentAdded)
        {
            for it in &snap.items {
                if let (Some(s), Some(c)) = (it.series, snap.primary(it).created.as_ref()) {
                    let e: &mut String = last_added.entry(s).or_default();
                    if c > e {
                        *e = c.clone();
                    }
                }
            }
        }
        let mut random = HashMap::new();
        if keys.iter().any(|(k, _)| *k == SortKey::Random) {
            use rand::Rng;
            let mut rng = rand::thread_rng();
            for i in candidates {
                random.insert(*i, rng.gen());
            }
        }
        SortCtx {
            snap,
            user,
            last_added,
            random,
        }
    }

    fn value(&self, idx: usize, key: SortKey) -> Option<V> {
        use SortKey::*;
        let it = &self.snap.items[idx];
        let e = self.snap.primary(it);
        let series_entry = || it.series.map(|s| self.snap.primary(&self.snap.items[s]));
        match key {
            SortName => Some(V::S(e.sort_name.clone())),
            Name => Some(V::S(e.name.clone())),
            DateCreated => e.created.clone().map(V::S),
            PremiereDate => e.premiere.clone().map(V::S),
            ProductionYear => e.year.map(V::I),
            CommunityRating => e.community.map(V::F),
            CriticRating => e.critic.map(V::F),
            OfficialRating => it.rating_score.map(V::I),
            Runtime => e.runtime.map(V::I),
            Random => self.random.get(&idx).map(|r| V::I(*r as i64)),
            IsFolder => Some(V::I(e.is_folder as i64)),
            SeriesSortName => series_entry()
                .map(|s| s.sort_name.clone())
                .or_else(|| e.series_name.as_ref().map(|n| n.to_lowercase()))
                .map(V::S),
            ParentIndexNumber => e.parent_index.map(V::I),
            IndexNumber => e.index.map(V::I),
            AiredEpisodeOrder => e
                .parent_index
                .map(|s| V::I(s * 100_000 + e.index.unwrap_or(0))),
            DateLastContentAdded => self
                .last_added
                .get(&idx)
                .cloned()
                .or_else(|| e.created.clone())
                .map(V::S),
            PlayCount => Some(V::I((self.user)(it).map_or(0, |u| u.play_count))),
            DatePlayed => (self.user)(it).and_then(|u| u.last_played).map(V::S),
            IsPlayed => Some(V::I((self.user)(it).is_some_and(|u| u.played) as i64)),
            IsUnplayed => Some(V::I(!(self.user)(it).is_some_and(|u| u.played) as i64)),
            IsFavoriteOrLiked => Some(V::I(
                (self.user)(it).is_some_and(|u| u.favorite || u.likes == Some(true)) as i64,
            )),
            SeriesDatePlayed => (self.user)(it).and_then(|u| u.last_played).map(V::S),
        }
    }

    /// Sort item indexes by `keys` (`true` = descending).
    pub fn sort(&self, items: &mut [usize], keys: &[(SortKey, bool)]) {
        let mut decorated: Vec<(Vec<Option<V>>, usize)> = items
            .iter()
            .map(|i| (keys.iter().map(|(k, _)| self.value(*i, *k)).collect(), *i))
            .collect();
        decorated.sort_by(|(va, a), (vb, b)| {
            for (n, (_, desc)) in keys.iter().enumerate() {
                let o = cmp_opt(&va[n], &vb[n]);
                let o = if *desc { o.reverse() } else { o };
                if o != Ordering::Equal {
                    return o;
                }
            }
            let (ia, ib) = (&self.snap.items[*a], &self.snap.items[*b]);
            self.snap
                .primary(ia)
                .sort_name
                .cmp(&self.snap.primary(ib).sort_name)
                .then_with(|| ia.id.cmp(&ib.id))
        });
        for (slot, (_, i)) in items.iter_mut().zip(decorated) {
            *slot = i;
        }
    }
}

/// Parse `SortBy` / `SortOrder` like Jellyfin: one order applies to every
/// key, otherwise orders pair up by position (missing = ascending).
/// `Err(key)` for a key the index cannot sort by.
pub fn parse_sort(by: Option<&str>, order: Option<&str>) -> Result<Vec<(SortKey, bool)>, String> {
    let orders: Vec<bool> = order
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().eq_ignore_ascii_case("descending"))
        .collect();
    let mut out = vec![];
    for (n, k) in by
        .unwrap_or("")
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .enumerate()
    {
        let key = SortKey::parse(k).ok_or_else(|| k.trim().to_string())?;
        let desc = match orders.len() {
            0 => false,
            1 => orders[0],
            _ => orders.get(n).copied().unwrap_or(false),
        };
        out.push((key, desc));
    }
    Ok(out)
}
