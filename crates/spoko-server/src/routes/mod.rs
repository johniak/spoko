mod devices;
mod events;
mod objects;
mod pairing;
mod pointers;
mod projects;

use axum::Router;
use axum::routing::{delete, get, post, put};

use crate::Shared;

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/devices/bootstrap", post(devices::bootstrap))
        .route("/account/recovery", get(devices::recovery))
        .route("/account/keyrings", put(devices::update_keyrings))
        .route("/devices", get(devices::list))
        .route("/devices/me/keyring", get(devices::my_keyring))
        .route("/devices/{id}", delete(devices::revoke))
        .route("/pairing/start", post(pairing::start))
        .route("/pairing/by-code/{code}", get(pairing::by_code))
        .route("/pairing/{id}", get(pairing::poll))
        .route("/pairing/{id}/approve", post(pairing::approve))
        .route("/projects", post(projects::create).get(projects::list))
        .route("/projects/{p}/objects/missing", post(objects::missing))
        .route("/projects/{p}/objects", post(objects::upload))
        .route("/projects/{p}/objects/get", post(objects::fetch))
        .route("/projects/{p}/pointers/{name}", get(pointers::get).put(pointers::put))
        .route("/projects/{p}/messages", post(events::post_message))
        .route("/projects/{p}/presence", get(events::presence))
        .route("/events", get(events::ws))
}
