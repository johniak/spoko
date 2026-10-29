//! Real OS / Docker connections (excluded from coverage; exercised by the docker CI job).

use std::sync::Arc;

use spoko_docker::api::DockerApi;
use spoko_docker::bollard_api::BollardApi;

use crate::docker_ext::DockerExt;

/// Volume sync against the local Docker daemon (socket / named pipe).
pub fn docker_ext() -> DockerExt {
    DockerExt::with(Arc::new(|| Ok(Arc::new(BollardApi::connect()?) as Arc<dyn DockerApi>)))
}

/// `spoko daemon install|uninstall`: changes the real login items (launchd / systemd / registry).
pub fn autostart(install: bool, out: &mut dyn std::io::Write) -> crate::commands::Res<()> {
    use spoko_client::os_glue::autostart;
    if install {
        autostart::install()?;
        writeln!(out, "daemon będzie startował po zalogowaniu")?;
    } else {
        autostart::uninstall()?;
        writeln!(out, "usunięto autostart daemona")?;
    }
    Ok(())
}
