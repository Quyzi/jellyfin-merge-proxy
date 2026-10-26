use axum::http::{HeaderMap, StatusCode};
use metrics::counter;
use serde_json::Value;

use crate::auth::{self, Session};
use crate::dto;
use crate::ids::hash_id;
use crate::password::Password;
use crate::telemetry::{names, Stage};
use crate::Shared;

use super::response::{err_resp, ok_json, q_get};
use super::{Client, Req};

pub fn session_from(st: &Shared, headers: &HeaderMap, query: Option<&str>) -> Option<Session> {
    let q: Vec<(String, String)> = query
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();
    let info = auth::client_auth(headers, &q);
    info.get("token").and_then(|t| st.sessions.get(t))
}

pub fn authenticate(st: &Shared, r: &Req, classified: &[&str]) -> Option<Client> {
    let _s = Stage::start("auth");
    let is_get = r.method == axum::http::Method::GET || r.method == axum::http::Method::HEAD;
    let anonymous_ok = is_get && classified.contains(&"images");
    let info = auth::client_auth(&r.headers, &r.query);
    let token = info.get("token");
    let session = token.and_then(|t| st.sessions.get(t));
    let outcome = match (&session, token) {
        (Some(_), _) => "valid",
        (None, Some(_)) if anonymous_ok => "anonymous",
        (None, Some(_)) => "invalid",
        (None, None) if anonymous_ok => "anonymous",
        (None, None) => "missing",
    };
    counter!(names::TOKEN_VALIDATIONS, "outcome" => outcome).increment(1);
    match session {
        Some(s) => Some(Client {
            token: s.token.clone(),
            user_id: s.user_id.clone(),
            session: Some(s),
        }),
        None if anonymous_ok => Some(anon_client(st)),
        None => None,
    }
}

pub fn anon_client(st: &Shared) -> Client {
    Client {
        token: String::new(),
        user_id: st.authenticator.ids().first().cloned().unwrap_or_default(),
        session: None,
    }
}

pub fn current_user(st: &Shared, c: &Client) -> Value {
    let name = st
        .authenticator
        .by_id(&c.user_id)
        .map(|u| u.name)
        .unwrap_or_default();
    dto::user_dto(&name, &c.user_id, &st.server_id, &st.cfg().proxy.name)
}

#[tracing::instrument(level = "debug", skip_all)]
pub async fn login(st: &Shared, r: &Req) -> axum::response::Response {
    let body: Value = serde_json::from_slice(&r.body).unwrap_or(Value::Null);
    let field = |names: &[&str]| -> Option<String> {
        body.as_object().and_then(|o| {
            o.iter()
                .find(|(k, _)| names.iter().any(|n| k.eq_ignore_ascii_case(n)))
                .and_then(|(_, v)| v.as_str().map(String::from))
        })
    };
    let denied = || err_resp(StatusCode::UNAUTHORIZED, "Invalid username or password");
    let Some(username) = field(&["Username", "Name"]) else {
        counter!(names::LOGINS, "outcome" => "bad_request").increment(1);
        return denied();
    };
    let pw = Password::new(field(&["Pw", "Password"]).unwrap_or_default());
    let auth = st.authenticator.clone();
    let name = username.clone();
    let res = tokio::task::spawn_blocking(move || auth.authenticate(&name, &pw)).await;
    let user = match res {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => {
            tracing::info!(user = %username, reason = e.reason(), "login failed");
            return denied();
        }
        Err(_) => return denied(),
    };
    let info = auth::client_auth(&r.headers, &[]);
    let token = auth::new_token();
    let s = Session {
        token: token.clone(),
        session_id: hash_id(&["session", &token]),
        user_name: user.name.clone(),
        user_id: user.id.clone(),
        client: info
            .get("client")
            .cloned()
            .unwrap_or_else(|| "Unknown".into()),
        device_name: info
            .get("device")
            .cloned()
            .unwrap_or_else(|| "Unknown".into()),
        device_id: info
            .get("deviceid")
            .cloned()
            .unwrap_or_else(|| hash_id(&["device", &token])),
        app_version: info.get("version").cloned().unwrap_or_default(),
        created: dto::now_iso(),
    };
    tracing::info!(user = %s.user_name, client = %s.client, device = %s.device_name, "user logged in");
    st.sessions.insert(s.clone());
    let user_dto = dto::user_dto(&user.name, &user.id, &st.server_id, &st.cfg().proxy.name);
    ok_json(dto::auth_result(user_dto, &s, &st.server_id))
}

pub fn display_prefs(st: &Shared, r: &Req, c: &Client) -> axum::response::Response {
    let id = r.segs[1].clone();
    let client = q_get(&r.query, "client").unwrap_or("emby").to_string();
    let key = format!("{}|{}|{}", c.user_id, id, client);
    if r.method == axum::http::Method::POST {
        if let Ok(v) = serde_json::from_slice::<Value>(&r.body) {
            st.display_prefs.lock().unwrap().insert(key, v);
        }
        return super::response::no_content();
    }
    let v = st
        .display_prefs
        .lock()
        .unwrap()
        .get(&key)
        .cloned()
        .unwrap_or_else(|| dto::display_prefs(&id, &client));
    ok_json(v)
}
