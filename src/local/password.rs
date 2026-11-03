//! Changing a proxy user's password: verify the current one, write a new
//! argon2 hash into the config file, reload, and revoke the other sessions
//! (`UserController.UpdateUserPassword`, `UserManager.ChangePassword`).

use crate::password::{Argon2id, HashedPassword, Password, PasswordHasher};
use crate::proxy::{err_resp, no_content, Client};
use crate::types::normalize_hex_id;
use crate::Shared;
use axum::http::StatusCode;
use axum::response::Response;
use serde_json::Value;

fn forbidden(msg: &str) -> Response {
    err_resp(StatusCode::FORBIDDEN, msg)
}

fn failed() -> Response {
    err_resp(
        StatusCode::INTERNAL_SERVER_ERROR,
        "Error processing request.",
    )
}

fn text(body: &Value, key: &str) -> String {
    body.as_object()
        .and_then(|o| o.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)))
        .and_then(|(_, v)| v.as_str())
        .unwrap_or_default()
        .to_string()
}

pub async fn change(st: &Shared, c: &Client, target: &str, body: &Value) -> Response {
    let _guard = st.local.password_lock.lock().await;
    let Some(target) = normalize_hex_id(target) else {
        return err_resp(StatusCode::NOT_FOUND, "Error processing request.");
    };
    let Some(user) = st.authenticator.by_id(&target) else {
        return err_resp(StatusCode::NOT_FOUND, "Error processing request.");
    };
    const DENIED: &str = "User is not allowed to update the password.";
    if target != c.user_id {
        return forbidden(DENIED);
    }
    let reset = body
        .as_object()
        .and_then(|o| {
            o.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("ResetPassword"))
        })
        .and_then(|(_, v)| v.as_bool())
        .unwrap_or(false);
    if reset {
        return forbidden(DENIED);
    }
    let auth = st.authenticator.clone();
    let name = user.name.clone();
    let current = Password::new(text(body, "CurrentPw"));
    let verified = tokio::task::spawn_blocking(move || auth.authenticate(&name, &current)).await;
    if !matches!(verified, Ok(Ok(_))) {
        return forbidden("Invalid user or password entered.");
    }
    let path = st
        .source
        .as_ref()
        .and_then(|s| s.path().map(|p| p.to_path_buf()));
    let name = user.name.clone();
    let new_pw = Password::new(text(body, "NewPw"));
    let written = tokio::task::spawn_blocking(move || -> Result<bool, String> {
        let phc: HashedPassword = Argon2id.hash(&new_pw).map_err(|e| e.to_string())?;
        let path = path.ok_or("no writable config file")?;
        crate::config_rewrite::set_user_password(&path, &name, phc.phc()).map_err(|e| e.to_string())
    })
    .await;
    match written {
        Ok(Ok(true)) => {}
        other => {
            tracing::warn!(user = %user.name, "storing the new password failed: {other:?}");
            return failed();
        }
    }
    if let Err(e) = st.reload(false).await {
        tracing::warn!(
            "password stored in the config file but reloading it failed ({e}); \
             it applies after the next successful reload or restart"
        );
        return failed();
    }
    for s in st
        .sessions
        .remove_where(|s| s.user_id == target && s.token != c.token)
    {
        st.local.live.remove(&s.session_id);
    }
    no_content()
}
