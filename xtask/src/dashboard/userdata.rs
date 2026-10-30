//! Per-user state row: user data changes, write-through to upstream copies
//! and the one-time import of upstream state.

use super::*;

pub(super) fn userdata(l: &mut Layout) {
    l.row("Per-user state");
    l.add(
        Panel::stat(
            "User states held",
            "Per-user item states (played, resume point, favourite, likes) kept by the proxy for all proxy users.",
            q(format!("max(jmp_userdata_items{{{I}}})"), ""),
        )
        .unit("short")
        .decimals(0)
        .no_value("Disabled")
        .size(4, 8),
    );
    l.add(
        Panel::timeseries(
            "Changes by operation",
            "played/unplayed, favorite/unfavorite, rating, update (UserData POST), start/progress/stopped (playback reports) and seed (imported from an upstream).",
            &[q(
                format!("sum by (op) ({})", rate("jmp_userdata_changes_total", I)),
                "{{op}}",
            )],
        )
        .stacked()
        .unit("ops")
        .size(10, 8),
    );
    l.add(
        Panel::timeseries(
            "Write-through to upstream copies",
            "Changes sent on to every server holding a copy. error: the upstream refused or was unreachable (the proxy's own state is still updated).",
            &[q(
                format!("sum by (outcome) ({})", rate("jmp_userdata_write_through_total", I)),
                "{{outcome}}",
            )],
        )
        .stacked()
        .unit("reqps")
        .overrides(palette(&[("ok", GOOD), ("error", BAD)]))
        .size(10, 8),
    );
    l.add(
        Panel::timeseries(
            "Imports and database errors",
            "One-time imports of upstream state per (user, server), and failed database writes of user data (the change is kept in memory and retried with the next change of that item).",
            &[
                q(
                    format!("sum by (outcome) (increase(jmp_userdata_seeds_total{{{I}}}[$__rate_interval]))"),
                    "import {{outcome}}",
                ),
                q(
                    format!("sum(increase(jmp_userdata_store_errors_total{{{I}}}[$__rate_interval]))"),
                    "db errors",
                ),
            ],
        )
        .bars()
        .unit("short")
        .decimals(0)
        .overrides(palette(&[("import ok", GOOD), ("import error", WARN), ("db errors", BAD)]))
        .size(24, 7),
    );
}
