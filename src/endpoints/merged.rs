//! Endpoints that answer for every server (recommendations, suggestions,
//! Live TV, name details, play-session routing) and the ones Jellyfin
//! restricts to administrators or Live TV managers, which proxy users never
//! are (empty 403, as Jellyfin's authorization handlers answer).

use super::{done, Flow, HandlerResult, Route};
use crate::handler;
use crate::proxy::{self, Client, Req};
use crate::Shared;
use axum::http::{Method, StatusCode};
use axum::response::IntoResponse;

pub fn routes() -> Vec<Route> {
    vec![
        Route::auth("movie-recommendations", handler!(recommendations))
            .at("movies/recommendations")
            .get(),
        Route::auth("suggestions", handler!(suggestions))
            .at("items/suggestions")
            .at("users/{u}/suggestions")
            .get(),
        Route::auth("livetv-channels", handler!(channels))
            .at("livetv/channels")
            .get(),
        Route::auth("livetv-programs", handler!(programs))
            .at("livetv/programs")
            .get(),
        Route::auth("livetv-programs-query", handler!(programs_query))
            .at("livetv/programs")
            .post(),
        Route::auth(
            "livetv-programs-recommended",
            handler!(programs_recommended),
        )
        .at("livetv/programs/recommended")
        .get(),
        Route::auth("livetv-recordings", handler!(recordings))
            .at("livetv/recordings")
            .get(),
        Route::auth("livetv-recording-folders", handler!(recording_folders))
            .at("livetv/recordings/folders")
            .get(),
        Route::auth("livetv-timers", handler!(timers))
            .at("livetv/timers")
            .get(),
        Route::auth("livetv-series-timers", handler!(series_timers))
            .at("livetv/seriestimers")
            .get(),
        Route::auth("livetv-info", handler!(livetv_info))
            .at("livetv/info")
            .get(),
        Route::auth("livetv-guide-info", handler!(livetv_guide_info))
            .at("livetv/guideinfo")
            .get(),
        // LiveTvManagement policy.
        Route::auth("livetv-recording-delete", handler!(forbidden))
            .at("livetv/recordings/*")
            .methods(&[Method::DELETE]),
        Route::auth("livetv-timer-create", handler!(forbidden))
            .at("livetv/timers")
            .at("livetv/seriestimers")
            .post(),
        Route::auth("livetv-timer-edit", handler!(forbidden))
            .at("livetv/timers/*")
            .at("livetv/seriestimers/*")
            .methods(&[Method::POST, Method::DELETE]),
        // RequiresElevation policy.
        Route::auth("livetv-admin-post", handler!(forbidden))
            .at("livetv/tuners/*/reset")
            .at("livetv/tunerhosts")
            .at("livetv/listingproviders")
            .at("livetv/channelmappings")
            .post(),
        Route::auth("livetv-admin-delete", handler!(forbidden))
            .at("livetv/tunerhosts")
            .at("livetv/listingproviders")
            .methods(&[Method::DELETE]),
        Route::auth("livetv-admin-get", handler!(forbidden))
            .at("livetv/tuners/discover")
            .at("livetv/tuners/discvover")
            .at("livetv/listingproviders/schedulesdirect/countries")
            .at("livetv/channelmappingoptions")
            .get(),
        Route::auth("name-detail", handler!(name_detail))
            .at("genres/*")
            .at("studios/*")
            .at("persons/*")
            .at("musicgenres/*")
            .at("artists/*")
            .at("genres/*/images/**")
            .at("studios/*/images/**")
            .at("persons/*/images/**")
            .at("musicgenres/*/images/**")
            .at("artists/*/images/**")
            .get(),
        Route::auth("active-encodings", handler!(stop_encoding))
            .at("videos/activeencodings")
            .methods(&[Method::DELETE]),
    ]
}

async fn recommendations(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::movie_recommendations(st, &r, c).await?)
}

async fn suggestions(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::suggestions(st, &r, c).await?)
}

macro_rules! fan_list_handlers {
    ($($name:ident => $spec:ident),* $(,)?) => {$(
        async fn $name(st: &Shared, r: Req, c: &Client) -> HandlerResult {
            done(proxy::fan_list(st, &r, c, &proxy::$spec).await?)
        }
    )*};
}

fan_list_handlers! {
    channels => LIVE_CHANNELS,
    programs => LIVE_PROGRAMS,
    programs_recommended => LIVE_RECOMMENDED,
    recordings => LIVE_RECORDINGS,
    recording_folders => LIVE_RECORDING_FOLDERS,
    timers => LIVE_TIMERS,
    series_timers => LIVE_SERIES_TIMERS,
}

async fn programs_query(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::livetv_programs(st, &r, c).await?)
}

async fn livetv_info(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::livetv_info(st, &r, c).await?)
}

async fn livetv_guide_info(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::livetv_guide_info(st, &r, c).await?)
}

async fn forbidden(_st: &Shared, _r: Req, _c: &Client) -> HandlerResult {
    done(StatusCode::FORBIDDEN.into_response())
}

/// `Genres/{name}` & co: forwarded to a server that has the name.
async fn name_detail(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    let second = r.segs[1].as_str();
    // Lists and instant mixes are other routes; an id is routed by owner.
    if second.eq_ignore_ascii_case("albumartists")
        || second.eq_ignore_ascii_case("instantmix")
        || st.ids.resolve(second).is_some()
    {
        return Ok(Flow::Next(r));
    }
    match proxy::name_server(st, &r, c).await {
        Some(server) => done(proxy::forward(st, server, r, c, None).await?),
        None => done(proxy::primary_forward(st, r, c).await?),
    }
}

async fn stop_encoding(st: &Shared, r: Req, c: &Client) -> HandlerResult {
    done(proxy::stop_encoding(st, r, c).await?)
}
