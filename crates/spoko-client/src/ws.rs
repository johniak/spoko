//! The account's event stream (WebSocket).

use std::io::ErrorKind;
use std::net::TcpStream;
use std::time::Duration;

use spoko_proto::{API_PREFIX, ServerEvent};
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::{ClientError, Result};

pub struct EventStream {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
}

fn tcp(ws: &WebSocket<MaybeTlsStream<TcpStream>>) -> Option<&TcpStream> {
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    }
}

impl EventStream {
    /// Connects to `/v1/events`. The first frame the server sends is the account presence,
    /// after which the subscription is live; callers should resync after every (re)connect.
    pub fn connect(server_url: &str, token: &str) -> Result<EventStream> {
        let url = format!("{}{API_PREFIX}/events", server_url.trim_end_matches('/')).replacen("http", "ws", 1);
        let mut req = url.into_client_request().map_err(|e| ClientError::Network(e.to_string()))?;
        req.headers_mut().insert("Authorization", format!("Bearer {token}").parse().map_err(|_| ClientError::Other("bad token".into()))?);
        let (ws, _) = tungstenite::connect(req).map_err(|e| match e {
            tungstenite::Error::Http(r) if r.status() == 401 => ClientError::Unauthorized,
            e => ClientError::Network(e.to_string()),
        })?;
        Ok(EventStream { ws })
    }

    /// Next event, or `None` after `timeout` without one.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<ServerEvent>> {
        if let Some(s) = tcp(&self.ws) {
            s.set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        }
        loop {
            match self.ws.read() {
                Ok(Message::Text(t)) => return serde_json::from_str(&t).map(Some).map_err(|e| ClientError::Server(e.to_string())),
                Ok(Message::Close(_)) => return Err(ClientError::Network("event stream closed".into())),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => return Ok(None),
                Err(e) => return Err(ClientError::Network(e.to_string())),
            }
        }
    }
}
