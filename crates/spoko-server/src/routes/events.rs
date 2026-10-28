use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Response;
use spoko_proto::{MessagePost, Presence, ServerEvent};
use uuid::Uuid;

use crate::Shared;
use crate::auth::AuthDevice;
use crate::error::ApiResult;

pub async fn post_message(State(s): State<Shared>, auth: AuthDevice, Path(p): Path<Uuid>, Json(req): Json<MessagePost>) -> ApiResult<StatusCode> {
    auth.check_project(&s, p)?;
    let event = ServerEvent::Message { project: p, from_device: auth.device_id, payload: req.payload };
    s.hub.publish(auth.account_id, Some(auth.device_id), event);
    Ok(StatusCode::NO_CONTENT)
}

pub async fn presence(State(s): State<Shared>, auth: AuthDevice, Path(p): Path<Uuid>) -> ApiResult<Json<Presence>> {
    auth.check_project(&s, p)?;
    Ok(Json(Presence { online_devices: s.hub.online(auth.account_id) }))
}

pub async fn ws(State(s): State<Shared>, auth: AuthDevice, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| run_socket(s, auth, socket))
}

fn announce_presence(s: &Shared, auth: AuthDevice, online: Vec<Uuid>) {
    let projects = s.db.list_projects(auth.account_id).unwrap_or_default();
    for p in projects {
        s.hub.publish(auth.account_id, None, ServerEvent::PresenceChanged { project: p.id, online_devices: online.clone() });
    }
}

async fn run_socket(s: Shared, auth: AuthDevice, mut socket: WebSocket) {
    let mut rx = s.hub.subscribe(auth.account_id);
    let online = s.hub.set_online(auth.account_id, auth.device_id, true);
    announce_presence(&s, auth, online);
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(20));
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(env) if env.from_device == Some(auth.device_id) => {}
                Ok(env) => {
                    let text = serde_json::to_string(&env.event).expect("event serializes");
                    if socket.send(Message::Text(text.into())).await.is_err() { break; }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    let online = s.hub.set_online(auth.account_id, auth.device_id, false);
    announce_presence(&s, auth, online);
}
