use super::*;

fn q(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Regression (real Jellyfin 12.3): ProviderIds are only returned when
/// requested, so dedupe must ask for them whatever the client sent.
#[test]
fn ensure_field_adds_or_appends() {
    let mut a = q(&[("Limit", "5")]);
    ensure_field(&mut a, "ProviderIds");
    assert_eq!(a, q(&[("Limit", "5"), ("Fields", "ProviderIds")]));
    let mut b = q(&[("fields", "PrimaryImageAspectRatio,Path")]);
    ensure_field(&mut b, "ProviderIds");
    assert_eq!(
        b,
        q(&[("fields", "PrimaryImageAspectRatio,Path,ProviderIds")])
    );
    let mut c = q(&[("Fields", "providerids")]);
    ensure_field(&mut c, "ProviderIds");
    assert_eq!(c, q(&[("Fields", "providerids")]));
    let mut d = q(&[("Fields", "")]);
    ensure_field(&mut d, "ProviderIds");
    assert_eq!(d, q(&[("Fields", "ProviderIds")]));
}

/// Regression (real Jellyfin 12.3): `/web/` must reach the upstream with
/// its trailing slash, or Jellyfin's `/web` -> `/web/` redirect loops.
#[test]
fn trailing_slash_is_kept() {
    let segs = vec!["web".to_string()];
    assert_eq!(path_with_query_slash(&segs, &[], true), "web/");
    assert_eq!(path_with_query_slash(&segs, &[], false), "web");
    assert_eq!(path_with_query_slash(&[], &[], true), "");
    assert_eq!(
        path_with_query_slash(&segs, &q(&[("a", "1")]), true),
        "web/?a=1"
    );
}
