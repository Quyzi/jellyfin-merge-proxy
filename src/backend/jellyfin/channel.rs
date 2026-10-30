//! The backend's [`AuthChannel`]: unauthenticated or token calls used for
//! logins, under the backend's device id or a per-user account's.

use super::*;

#[async_trait]
impl AuthChannel for JellyfinBackend {
    fn server_name(&self) -> &str {
        &self.name
    }
    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), BackendError> {
        self.call_from(&self.device_id, method, path, token, body)
            .await
    }
}

/// The backend's [`AuthChannel`] under another device id (per-user accounts).
pub(crate) struct DeviceChannel<'a> {
    pub backend: &'a JellyfinBackend,
    pub device: &'a str,
}

#[async_trait]
impl AuthChannel for DeviceChannel<'_> {
    fn server_name(&self) -> &str {
        &self.backend.name
    }
    async fn call(
        &self,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), BackendError> {
        self.backend
            .call_from(self.device, method, path, token, body)
            .await
    }
}

impl JellyfinBackend {
    pub(super) fn channel<'a>(
        &'a self,
        a: &'a crate::backend::accounts::Account,
    ) -> DeviceChannel<'a> {
        DeviceChannel {
            backend: self,
            device: &a.device_id,
        }
    }

    pub(super) async fn call_from(
        &self,
        device: &str,
        method: Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> Result<(StatusCode, Value), BackendError> {
        let mut h = if token.is_some() || body.is_some() {
            self.auth_headers_for(token, device)
        } else {
            HeaderMap::new()
        };
        let body = body.map(|b| {
            h.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            Bytes::from(serde_json::to_vec(&b).unwrap_or_default())
        });
        let req = self.build_request(method, path, h, body)?;
        let resp = self.raw_send(req, false).await.map_err(|e| {
            let e = e.without_url();
            self.mark_down(&e.to_string());
            BackendError::Unreachable {
                server: self.name.clone(),
                msg: e.to_string(),
            }
        })?;
        let status = resp.status();
        let v = resp.json::<Value>().await.unwrap_or(Value::Null);
        Ok((status, v))
    }
}
