use std::collections::HashMap;
use std::io::{Read, Write};

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerInfo {
    pub id: String,
    pub image: String,
    pub image_id: String,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub arch: String,
    pub os: String,
    pub tags: Vec<String>,
}

/// The Docker operations volume sync needs (real: [`crate::bollard_api::BollardApi`]).
pub trait DockerApi: Send + Sync {
    /// Full volume name for a logical name from `.spokoconfig`.
    fn resolve_volume(&self, logical: &str, project_hint: &str) -> Result<String>;
    fn containers_using(&self, volume: &str) -> Result<Vec<ContainerInfo>>;
    fn image_info(&self, image_id: &str) -> Result<ImageInfo>;
    fn stop(&self, id: &str) -> Result<()>;
    fn start(&self, id: &str) -> Result<()>;
    /// Tar of the volume's contents (entries under `spoko-vol/`).
    fn export_volume(&self, volume: &str, image: &str, out: &mut dyn Write) -> Result<()>;
    /// Replaces the volume's contents with a tar produced by `export_volume`.
    fn import_volume(&self, volume: &str, image: &str, tar: &mut dyn Read) -> Result<()>;
    fn exec(&self, container: &str, cmd: &[&str], stdin: Option<&mut dyn Read>, stdout: Option<&mut dyn Write>) -> Result<i32>;
}

pub const COMPOSE_VOLUME_LABEL: &str = "com.docker.compose.volume";
pub const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// Exact name first, then the unique volume carrying compose's logical-name label
/// (several → the one of compose project `project_hint`, else ambiguous).
pub fn pick_volume(logical: &str, project_hint: &str, volumes: &[(String, HashMap<String, String>)]) -> Result<String> {
    if volumes.iter().any(|(n, _)| n == logical) {
        return Ok(logical.to_owned());
    }
    let labeled: Vec<&(String, HashMap<String, String>)> = volumes.iter().filter(|(_, l)| l.get(COMPOSE_VOLUME_LABEL).map(String::as_str) == Some(logical)).collect();
    match labeled.as_slice() {
        [] => Err(Error::NotFound(logical.to_owned())),
        [(n, _)] => Ok(n.clone()),
        many => match many.iter().find(|(_, l)| l.get(COMPOSE_PROJECT_LABEL).map(String::as_str) == Some(project_hint)) {
            Some((n, _)) => Ok(n.clone()),
            None => Err(Error::Ambiguous { logical: logical.to_owned(), candidates: many.iter().map(|(n, _)| n.clone()).collect() }),
        },
    }
}
