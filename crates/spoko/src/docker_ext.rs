//! Wires Docker volume sync into the daemon.

use std::sync::Arc;

use spoko_client::daemon::Extensions;
use spoko_client::daemon::ipc::{Request, Response};
use spoko_client::engine::Engine;
use spoko_core::config::VolumeCfg;
use spoko_docker::api::DockerApi;
use spoko_docker::volume::{self, PullOutcome, VolumeCtx};

/// Opens a Docker connection when a volume operation needs one.
pub type Connect = Arc<dyn Fn() -> spoko_docker::Result<Arc<dyn DockerApi>> + Send + Sync>;

pub struct DockerExt {
    connect: Connect,
}

impl DockerExt {
    pub fn with(connect: Connect) -> DockerExt {
        DockerExt { connect }
    }

    /// Push (`pull = None`) or pull (`Some((force, fresh))`) of the selected volumes; one line per volume.
    pub fn run_volumes(&self, e: &Engine, name: Option<&str>, pull: Option<(bool, bool)>) -> spoko_docker::Result<String> {
        if selected(e, name).is_empty() {
            return Err(spoko_docker::Error::Other("brak takich wolumenów w .spokoconfig ([[docker.volume]])".into()));
        }
        run_volumes_with(&*(self.connect)()?, e, name, pull)
    }
}

/// Compose's project name for a directory (lowercase, `[a-z0-9_-]`).
pub fn compose_project(e: &Engine) -> String {
    let name = e.root.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect()
}

pub fn selected<'a>(e: &'a Engine, name: Option<&str>) -> Vec<&'a VolumeCfg> {
    e.config().docker.volume.iter().filter(|v| name.is_none_or(|n| n == v.name)).collect()
}

fn run_volumes_with(docker: &dyn DockerApi, e: &Engine, name: Option<&str>, pull: Option<(bool, bool)>) -> spoko_docker::Result<String> {
    let vols = selected(e, name);
    let hint = compose_project(e);
    let ctx = VolumeCtx {
        docker,
        api: &e.ctx.api,
        keyring: &e.ctx.keyring,
        project: e.project_id,
        project_hint: &hint,
        device_id: e.ctx.device_id,
        device_name: &e.ctx.device_name,
        now: e.now_public(),
    };
    let mut lines = Vec::new();
    for v in vols {
        let line = match pull {
            None => {
                let r = volume::push(&ctx, v)?;
                format!("{}: wysłano ({} nowych fragmentów)", v.name, r.uploaded_chunks)
            }
            Some((force, fresh)) => {
                let out = if fresh { volume::pull_fresh(&ctx, v, std::time::Duration::from_secs(120), std::time::Duration::from_secs(1))? } else { volume::pull(&ctx, v, force)? };
                match out {
                    PullOutcome::Restored(m) => format!("{}: przywrócono stan z {} ({})", v.name, m.device_name, m.created_at),
                    PullOutcome::NoSnapshot => format!("{}: brak snapshotu na serwerze", v.name),
                    PullOutcome::MismatchWarned(ms) => {
                        let list: Vec<String> = ms.iter().map(ToString::to_string).collect();
                        format!("{}: NIE przywrócono — niezgodność: {}. Użyj --force, jeśli na pewno.", v.name, list.join("; "))
                    }
                }
            }
        };
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

impl Extensions for DockerExt {
    fn on_message(&self, e: &mut Engine, payload: &[u8]) {
        let Some(cfg) = volume::requested_volume(&e.ctx.keyring, payload, &e.config().docker.volume).cloned() else { return };
        let _ = self.run_volumes(e, Some(&cfg.name), None);
    }

    fn on_local_activity(&self, e: &mut Engine) {
        if !e.config().docker.volume.is_empty() {
            let _ = volume::switch_detected(e);
        }
    }

    fn handle(&self, e: &mut Engine, req: &Request) -> Option<Response> {
        let r = match req {
            Request::VolumePush { volume, .. } => self.run_volumes(e, volume.as_deref(), None),
            Request::VolumePull { volume, force, fresh, .. } => self.run_volumes(e, volume.as_deref(), Some((*force, *fresh))),
            _ => return None,
        };
        Some(match r {
            Ok(text) => Response::Text { text },
            Err(err) => Response::Error { message: err.to_string() },
        })
    }
}
