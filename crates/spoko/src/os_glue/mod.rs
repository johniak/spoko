//! Real OS / Docker connections (excluded from coverage; exercised by the docker CI job).

use std::sync::Arc;

use spoko_docker::api::DockerApi;
use spoko_docker::bollard_api::BollardApi;

use crate::docker_ext::DockerExt;

/// Volume sync against the local Docker daemon (socket / named pipe).
pub fn docker_ext() -> DockerExt {
    DockerExt::with(Arc::new(|| Ok(Arc::new(BollardApi::connect()?) as Arc<dyn DockerApi>)))
}
