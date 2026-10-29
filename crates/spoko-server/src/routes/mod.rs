mod devices;
mod events;
mod objects;
mod pointers;
mod projects;

use axum::Router;
use axum::routing::{delete, get, post};

use crate::Shared;

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/auth/challenge", post(devices::challenge))
        .route("/auth/login", post(devices::login))
        .route("/account/keyring", get(devices::keyring).put(devices::store_keyring))
        .route("/devices", get(devices::list))
        .route("/devices/{id}", delete(devices::revoke))
        .route("/projects", post(projects::create).get(projects::list))
        .route("/projects/{p}/objects/missing", post(objects::missing))
        .route("/projects/{p}/objects", post(objects::upload))
        .route("/projects/{p}/objects/get", post(objects::fetch))
        .route("/projects/{p}/pointers/{name}", get(pointers::get).put(pointers::put))
        .route("/projects/{p}/messages", post(events::post_message))
        .route("/projects/{p}/presence", get(events::presence))
        .route("/events", get(events::ws))
}
