//! [`MediaBackend`] implementation for [`JellyfinBackend`].

use super::*;

#[async_trait]
impl MediaBackend for JellyfinBackend {
    fn id(&self) -> ServerId {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn kind(&self) -> &'static str {
        "jellyfin"
    }
    fn priority(&self) -> i64 {
        self.priority.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn location(&self) -> &BackendLocation {
        &self.location
    }
    fn is_down(&self) -> bool {
        matches!(*self.down_until.lock().unwrap(), Some(t) if t > Instant::now())
    }

    async fn ensure_login(&self) -> Result<(), BackendError> {
        if self.state.read().await.token.is_some() {
            return Ok(());
        }
        self.login(None).await
    }

    async fn session(&self) -> SessionInfo {
        let st = self.state.read().await;
        SessionInfo {
            user_id: st.user_id.clone(),
            modern: st
                .version
                .as_deref()
                .map(|v| version_at_least(v, 10, 9))
                .unwrap_or(true),
        }
    }

    async fn status(&self) -> BackendStatus {
        let st = self.state.read().await.clone();
        BackendStatus {
            name: self.name.clone(),
            kind: self.kind(),
            url: self.location.base.clone(),
            priority: self.priority(),
            healthy: !self.is_down(),
            logged_in: st.token.is_some(),
            credentials: self.credentials.read().unwrap().kind(),
            modern_api: st
                .version
                .as_deref()
                .map(|v| version_at_least(v, 10, 9))
                .unwrap_or(true),
            version: st.version,
            user_id: st.user_id,
            token_age_secs: st.obtained.map(|t| t.elapsed().as_secs()),
            user_accounts: self.accounts.len(),
        }
    }

    async fn report_gauges(&self) {
        let st = self.state.read().await;
        if let Some(t) = st.obtained {
            gauge!(names::UPSTREAM_TOKEN_AGE, "server" => self.name.clone())
                .set(t.elapsed().as_secs_f64());
        }
        gauge!(names::UPSTREAM_UP, "server" => self.name.clone()).set(if self.is_down() {
            0.0
        } else {
            1.0
        });
    }

    fn in_flight(&self) -> usize {
        self.in_flight.count()
    }

    /// Ends a password login's upstream session (`POST Sessions/Logout`).
    /// API keys are left alone: logging out with one would revoke the key.
    async fn retire(&self) {
        self.retired
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let token = self.state.read().await.token.clone();
        let is_password = self.credentials.read().unwrap().kind() == "password";
        *self.state.write().await = AuthState::default();
        if let Some(token) = token.filter(|_| is_password) {
            self.logout(&token, &self.device_id).await;
        }
        self.logout_accounts(self.accounts.all()).await;
    }

    fn has_account(&self, user: &str) -> bool {
        self.accounts.get(user).is_some()
    }

    async fn login_as(&self, user: Option<&str>) -> Result<SessionInfo, BackendError> {
        self.ensure_login().await?;
        let shared = self.session().await;
        let Some(a) = user.and_then(|u| self.accounts.get(u)) else {
            return Ok(shared);
        };
        let l = a.login(&self.channel(&a), None).await?;
        Ok(SessionInfo {
            user_id: l.user_id,
            modern: shared.modern,
        })
    }

    /// Retries once after re-login on 401.
    #[tracing::instrument(level = "debug", skip_all, fields(upstream = %self.name, method = %req.method, streaming = req.streaming))]
    async fn send(&self, req: BackendRequest) -> Result<BackendResponse, BackendError> {
        // Counted until the response body is finished (see `retire`).
        let guard = self.in_flight.guard();
        if self.is_down() || self.retired.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(BackendError::Down {
                server: self.name.clone(),
            });
        }
        self.ensure_login().await?;
        let account = req.as_user.as_deref().and_then(|u| self.accounts.get(u));
        let mut attempt = 0;
        loop {
            let token = match &account {
                Some(a) => Some(
                    a.login(&self.channel(a), None)
                        .await?
                        .token
                        .expose()
                        .to_string(),
                ),
                None => self.token().await,
            };
            let mut h = req.headers.clone();
            let device = account.as_ref().map_or(&self.device_id, |a| &a.device_id);
            h.extend(self.auth_headers_for(token.as_deref(), device));
            let r =
                self.build_request(req.method.clone(), &req.path_and_query, h, req.body.clone())?;
            let resp = match self.raw_send(r, req.streaming).await {
                Ok(r) => r,
                Err(e) => {
                    if e.is_connect() {
                        self.mark_down(&e.without_url().to_string());
                        return Err(BackendError::Transport {
                            server: self.name.clone(),
                            kind: TransportKind::Connect,
                            msg: "connection failed".into(),
                        });
                    }
                    return Err(transport_err(&self.name, e));
                }
            };
            if resp.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                attempt += 1;
                tracing::info!(upstream = %self.name, "401 from upstream, re-authenticating");
                counter!(names::UPSTREAM_REAUTH, "server" => self.name.clone()).increment(1);
                counter!(names::UPSTREAM_RETRIES, "server" => self.name.clone(), "reason" => "unauthorized").increment(1);
                match &account {
                    Some(a) => {
                        a.login(&self.channel(a), token.as_deref()).await?;
                    }
                    None => self.login(token.as_deref()).await?,
                }
                continue;
            }
            let status = resp.status();
            let headers = resp.headers().clone();
            let server = self.name.clone();
            let body = resp
                .bytes_stream()
                .map_err(move |e| transport_err(&server, e));
            return Ok(BackendResponse {
                status,
                headers,
                body: track_body(Box::pin(body), guard),
            });
        }
    }
}
