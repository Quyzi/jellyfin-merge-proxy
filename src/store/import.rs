//! One-time import of the files older versions kept in `data_dir`:
//! `ids.tsv` (`server\tupstream_id`), `siblings.tsv` (`id\tid`) and
//! `sessions.json`. Everything is written in one batch, then each file is
//! renamed to `<name>.migrated` so the import never runs twice; the
//! renamed files stay as a backup.

use super::*;
use crate::ids::item_proxy_id;
use crate::telemetry::names;
use std::path::Path;

pub const IDS_FILE: &str = "ids.tsv";
pub const SIBLINGS_FILE: &str = "siblings.tsv";
pub const SESSIONS_FILE: &str = "sessions.json";

/// What [`import_legacy`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub ids: usize,
    pub links: usize,
    pub sessions: usize,
    /// The files that were imported, now renamed to `*.migrated`.
    pub files: Vec<PathBuf>,
}

fn read(path: &Path) -> Result<Option<String>, StoreError> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(StoreError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// `ids.tsv` rows with the proxy id each one had in the old build.
pub fn parse_ids(text: &str) -> Vec<IdRow> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut it = line.splitn(2, '\t');
        if let (Some(name), Some(id)) = (it.next(), it.next()) {
            if let Some(upstream_id) = UpstreamItemId::parse(id) {
                out.push(IdRow {
                    proxy_id: item_proxy_id(name, &upstream_id),
                    server: name.to_string(),
                    upstream_id,
                });
            }
        }
    }
    out
}

pub fn parse_links(text: &str) -> Vec<(ProxyItemId, ProxyItemId)> {
    text.lines()
        .filter_map(|line| {
            let mut it = line.split('\t');
            Some((
                ProxyItemId::parse(it.next()?)?,
                ProxyItemId::parse(it.next()?)?,
            ))
        })
        .collect()
}

/// `<file>.migrated`, or `<file>.migrated.N` if that already exists.
fn migrated_name(path: &Path) -> PathBuf {
    let base = PathBuf::from(format!("{}.migrated", path.display()));
    let mut candidate = base.clone();
    let mut n = 1;
    while candidate.exists() {
        candidate = PathBuf::from(format!("{}.{n}", base.display()));
        n += 1;
    }
    candidate
}

/// Import the legacy files in `dir` into `store` and rename them.
/// Returns `None` when there was nothing to import. A `sessions.json`
/// that cannot be parsed is left in place (and logged) rather than lost.
pub fn import_legacy(store: &dyn Store, dir: &Path) -> Result<Option<Imported>, StoreError> {
    let mut batch = ImportBatch::default();
    let mut files = Vec::new();
    let ids_path = dir.join(IDS_FILE);
    if let Some(t) = read(&ids_path)? {
        batch.ids = parse_ids(&t);
        files.push(ids_path);
    }
    let links_path = dir.join(SIBLINGS_FILE);
    if let Some(t) = read(&links_path)? {
        batch.links = parse_links(&t);
        files.push(links_path);
    }
    let sessions_path = dir.join(SESSIONS_FILE);
    if let Some(t) = read(&sessions_path)? {
        match serde_json::from_str::<Vec<Session>>(&t) {
            Ok(list) => {
                batch.sessions = list;
                files.push(sessions_path);
            }
            Err(e) => tracing::warn!(
                path = %sessions_path.display(),
                "not importing unreadable sessions file: {e}"
            ),
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    store.import(&batch)?;
    for (kind, n) in [
        ("ids", batch.ids.len()),
        ("siblings", batch.links.len()),
        ("sessions", batch.sessions.len()),
    ] {
        metrics::counter!(names::DB_IMPORTED_ROWS, "kind" => kind).increment(n as u64);
    }
    for f in &files {
        let to = migrated_name(f);
        std::fs::rename(f, &to).map_err(|source| StoreError::Io {
            path: f.clone(),
            source,
        })?;
    }
    let r = Imported {
        ids: batch.ids.len(),
        links: batch.links.len(),
        sessions: batch.sessions.len(),
        files,
    };
    tracing::info!(
        ids = r.ids,
        sibling_links = r.links,
        sessions = r.sessions,
        "imported legacy state files into the {} store (renamed to *.migrated)",
        store.kind()
    );
    Ok(Some(r))
}

#[cfg(test)]
mod tests;
