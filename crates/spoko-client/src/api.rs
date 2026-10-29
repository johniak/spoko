//! Blocking HTTP client for the spoko server API.

use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;
use spoko_crypto::ObjectId;
use spoko_proto::*;
use ureq::Agent;
use uuid::Uuid;

use crate::transfer::ObjectServer;
use crate::{ClientError, Result};

#[derive(Clone)]
pub struct ApiClient {
    base: String,
    token: Option<String>,
    agent: Agent,
}

pub enum PollResult {
    Status(PairingStatus),
    Gone,
}

const MAX_BODY: u64 = (MAX_BATCH_BYTES as u64) * 2;

impl ApiClient {
    pub fn new(server_url: &str, token: Option<String>) -> ApiClient {
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        ApiClient {
            base: format!("{}{API_PREFIX}", server_url.trim_end_matches('/')),
            token,
            agent,
        }
    }

    pub fn with_token(&self, token: &str) -> ApiClient {
        ApiClient {
            base: self.base.clone(),
            token: Some(token.to_owned()),
            agent: self.agent.clone(),
        }
    }

    fn auth<B>(
        &self,
        req: ureq::RequestBuilder<B>,
        token: Option<&str>,
    ) -> ureq::RequestBuilder<B> {
        match token.or(self.token.as_deref()) {
            Some(t) => req.header("Authorization", format!("Bearer {t}")),
            None => req,
        }
    }

    fn check(
        resp: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    ) -> Result<(u16, Vec<u8>)> {
        let mut r = resp.map_err(|e| ClientError::Network(e.to_string()))?;
        let status = r.status().as_u16();
        let body = r
            .body_mut()
            .with_config()
            .limit(MAX_BODY)
            .read_to_vec()
            .map_err(|e| ClientError::Network(e.to_string()))?;
        match status {
            200..=299 | 404 | 409 | 410 => Ok((status, body)),
            401 => Err(ClientError::Unauthorized),
            s => Err(ClientError::Server(format!(
                "HTTP {s}: {}",
                String::from_utf8_lossy(&body)
            ))),
        }
    }

    fn json<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
        serde_json::from_slice(body).map_err(|e| ClientError::Server(format!("bad response: {e}")))
    }

    fn get_raw(&self, path: &str, token: Option<&str>) -> Result<(u16, Vec<u8>)> {
        Self::check(
            self.auth(self.agent.get(format!("{}{path}", self.base)), token)
                .call(),
        )
    }

    fn get_ok<T: DeserializeOwned>(&self, path: &str, token: Option<&str>) -> Result<Option<T>> {
        match self.get_raw(path, token)? {
            (404, _) => Ok(None),
            (_, b) => Ok(Some(Self::json(&b)?)),
        }
    }

    fn send_json<B: Serialize>(
        &self,
        method: &str,
        path: &str,
        body: &B,
        token: Option<&str>,
    ) -> Result<(u16, Vec<u8>)> {
        let url = format!("{}{path}", self.base);
        let req = match method {
            "POST" => self.agent.post(url),
            _ => self.agent.put(url),
        };
        Self::check(self.auth(req, token).send_json(body))
    }

    fn expect_ok(r: (u16, Vec<u8>)) -> Result<Vec<u8>> {
        match r {
            (200..=299, b) => Ok(b),
            (s, b) => Err(ClientError::Server(format!(
                "HTTP {s}: {}",
                String::from_utf8_lossy(&b)
            ))),
        }
    }

    pub fn bootstrap(
        &self,
        bootstrap_token: &str,
        req: &BootstrapRequest,
    ) -> Result<DeviceCredentials> {
        Self::json(&Self::expect_ok(self.send_json(
            "POST",
            "/devices/bootstrap",
            req,
            Some(bootstrap_token),
        )?)?)
    }

    pub fn recovery(&self, bootstrap_token: &str) -> Result<RecoveryBlob> {
        self.get_ok("/account/recovery", Some(bootstrap_token))?
            .ok_or_else(|| ClientError::Other("no recovery key stored for this account".into()))
    }

    pub fn devices(&self) -> Result<Vec<DeviceInfo>> {
        Ok(self.get_ok("/devices", None)?.unwrap_or_default())
    }

    pub fn my_keyring(&self) -> Result<WrappedKeyring> {
        self.get_ok("/devices/me/keyring", None)?
            .ok_or_else(|| ClientError::Server("keyring missing".into()))
    }

    pub fn revoke(&self, device: Uuid) -> Result<()> {
        let r = Self::check(
            self.auth(
                self.agent.delete(format!("{}/devices/{device}", self.base)),
                None,
            )
            .call(),
        )?;
        Self::expect_ok(r).map(|_| ())
    }

    pub fn update_keyrings(&self, upd: &KeyringUpdate) -> Result<()> {
        Self::expect_ok(self.send_json("PUT", "/account/keyrings", upd, None)?).map(|_| ())
    }

    pub fn pairing_start(&self, req: &PairingStart) -> Result<PairingStarted> {
        Self::json(&Self::expect_ok(self.send_json(
            "POST",
            "/pairing/start",
            req,
            None,
        )?)?)
    }

    pub fn pairing_poll(&self, id: Uuid, secret: &str) -> Result<PollResult> {
        match self.get_raw(&format!("/pairing/{id}?secret={secret}"), None)? {
            (410, _) => Ok(PollResult::Gone),
            (404, _) => Err(ClientError::Other("pairing not found".into())),
            (_, b) => Ok(PollResult::Status(Self::json(&b)?)),
        }
    }

    pub fn pairing_by_code(&self, code: &str) -> Result<Option<PairingInfo>> {
        self.get_ok(&format!("/pairing/by-code/{code}"), None)
    }

    pub fn pairing_approve(&self, id: Uuid, wrapped_keyring: Vec<u8>) -> Result<()> {
        Self::expect_ok(self.send_json(
            "POST",
            &format!("/pairing/{id}/approve"),
            &PairingApprove { wrapped_keyring },
            None,
        )?)
        .map(|_| ())
    }

    pub fn create_project(&self, encrypted_meta: &[u8]) -> Result<ProjectInfo> {
        Self::json(&Self::expect_ok(self.send_json(
            "POST",
            "/projects",
            &ProjectCreate {
                encrypted_meta: encrypted_meta.to_vec(),
            },
            None,
        )?)?)
    }

    pub fn projects(&self) -> Result<Vec<ProjectInfo>> {
        Ok(self.get_ok("/projects", None)?.unwrap_or_default())
    }

    pub fn get_pointer(&self, project: Uuid, name: &str) -> Result<Option<Pointer>> {
        self.get_ok(&format!("/projects/{project}/pointers/{name}"), None)
    }

    /// `Ok(Err(current))` when the compare-and-swap lost.
    pub fn put_pointer(
        &self,
        project: Uuid,
        name: &str,
        expected: Option<u64>,
        value: &[u8],
    ) -> Result<std::result::Result<Pointer, Pointer>> {
        let body = PointerPut {
            expected_version: expected,
            value: value.to_vec(),
        };
        match self.send_json(
            "PUT",
            &format!("/projects/{project}/pointers/{name}"),
            &body,
            None,
        )? {
            (409, b) => Ok(Err(Self::json(&b)?)),
            r => Ok(Ok(Self::json(&Self::expect_ok(r)?)?)),
        }
    }

    pub fn post_message(&self, project: Uuid, payload: &[u8]) -> Result<()> {
        Self::expect_ok(self.send_json(
            "POST",
            &format!("/projects/{project}/messages"),
            &MessagePost {
                payload: payload.to_vec(),
            },
            None,
        )?)
        .map(|_| ())
    }

    pub fn presence(&self, project: Uuid) -> Result<Presence> {
        self.get_ok(&format!("/projects/{project}/presence"), None)?
            .ok_or_else(|| ClientError::Server("project not found".into()))
    }

    pub fn objects(&self, project: Uuid) -> ProjectObjects {
        ProjectObjects {
            api: self.clone(),
            project,
        }
    }
}

/// Object endpoints of one project.
pub struct ProjectObjects {
    api: ApiClient,
    project: Uuid,
}

impl ObjectServer for ProjectObjects {
    fn missing(&self, ids: &[ObjectId]) -> Result<Vec<ObjectId>> {
        let r = self.api.send_json(
            "POST",
            &format!("/projects/{}/objects/missing", self.project),
            &MissingRequest { ids: ids.to_vec() },
            None,
        )?;
        Ok(ApiClient::json::<MissingResponse>(&ApiClient::expect_ok(r)?)?.ids)
    }

    fn put(&self, batch: &[(ObjectId, Vec<u8>)]) -> Result<()> {
        let req = self.api.auth(
            self.api.agent.post(format!(
                "{}/projects/{}/objects",
                self.api.base, self.project
            )),
            None,
        );
        let r = ApiClient::check(
            req.header("Content-Type", "application/octet-stream")
                .send(&encode_batch(batch)[..]),
        )?;
        ApiClient::expect_ok(r).map(|_| ())
    }

    fn get(&self, ids: &[ObjectId]) -> Result<Vec<(ObjectId, Vec<u8>)>> {
        let r = self.api.send_json(
            "POST",
            &format!("/projects/{}/objects/get", self.project),
            &MissingRequest { ids: ids.to_vec() },
            None,
        )?;
        decode_batch(&ApiClient::expect_ok(r)?).map_err(|e| ClientError::Server(e.to_string()))
    }
}
