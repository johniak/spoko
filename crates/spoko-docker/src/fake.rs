//! In-memory Docker for tests: volumes are byte blobs, containers are records.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;

use crate::api::{COMPOSE_VOLUME_LABEL, ContainerInfo, DockerApi, ImageInfo, pick_volume};
use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct FakeContainer {
    pub info: ContainerInfo,
    pub volume: String,
}

#[derive(Default)]
pub struct FakeDocker {
    pub volumes: Mutex<HashMap<String, Vec<u8>>>,
    pub labels: Mutex<HashMap<String, HashMap<String, String>>>,
    pub containers: Mutex<Vec<FakeContainer>>,
    pub images: Mutex<HashMap<String, ImageInfo>>,
    /// Calls in order: `stop <id>`, `start <id>`, `export <vol>`, `import <vol>`, `exec <id> <cmd>`.
    pub log: Mutex<Vec<String>>,
}

/// Prefix the fake's `exec` puts before the volume bytes it "dumps".
pub const DUMP_PREFIX: &[u8] = b"DUMP:";

impl FakeDocker {
    /// A compose-style volume `<project>_<logical>` used by one running container.
    pub fn with_volume(project: &str, logical: &str, data: &[u8], image: &str, arch: &str) -> FakeDocker {
        let d = FakeDocker::default();
        let name = format!("{project}_{logical}");
        d.volumes.lock().unwrap().insert(name.clone(), data.to_vec());
        d.labels.lock().unwrap().insert(name.clone(), HashMap::from([(COMPOSE_VOLUME_LABEL.to_owned(), logical.to_owned())]));
        d.images.lock().unwrap().insert(format!("sha256:{image}"), ImageInfo { arch: arch.into(), os: "linux".into(), tags: vec![image.into()] });
        d.containers.lock().unwrap().push(FakeContainer {
            info: ContainerInfo { id: format!("{project}-db"), image: image.into(), image_id: format!("sha256:{image}"), running: true },
            volume: name,
        });
        d
    }

    pub fn volume(&self, name: &str) -> Option<Vec<u8>> {
        self.volumes.lock().unwrap().get(name).cloned()
    }

    pub fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    fn record(&self, s: String) {
        self.log.lock().unwrap().push(s);
    }

    fn set_running(&self, id: &str, running: bool) -> Result<()> {
        let mut cs = self.containers.lock().unwrap();
        let c = cs.iter_mut().find(|c| c.info.id == id).ok_or_else(|| Error::Docker(format!("no container {id}")))?;
        c.info.running = running;
        Ok(())
    }
}

impl DockerApi for FakeDocker {
    fn resolve_volume(&self, logical: &str, project_hint: &str) -> Result<String> {
        let labels = self.labels.lock().unwrap();
        let vols: Vec<(String, HashMap<String, String>)> = self.volumes.lock().unwrap().keys().map(|n| (n.clone(), labels.get(n).cloned().unwrap_or_default())).collect();
        pick_volume(logical, project_hint, &vols)
    }

    fn containers_using(&self, volume: &str) -> Result<Vec<ContainerInfo>> {
        Ok(self.containers.lock().unwrap().iter().filter(|c| c.volume == volume).map(|c| c.info.clone()).collect())
    }

    fn image_info(&self, image_id: &str) -> Result<ImageInfo> {
        self.images.lock().unwrap().get(image_id).cloned().ok_or_else(|| Error::Docker(format!("no image {image_id}")))
    }

    fn stop(&self, id: &str) -> Result<()> {
        self.record(format!("stop {id}"));
        self.set_running(id, false)
    }

    fn start(&self, id: &str) -> Result<()> {
        self.record(format!("start {id}"));
        self.set_running(id, true)
    }

    fn export_volume(&self, volume: &str, _image: &str, out: &mut dyn Write) -> Result<()> {
        self.record(format!("export {volume}"));
        let running = self.containers.lock().unwrap().iter().any(|c| c.volume == volume && c.info.running);
        if running {
            self.record("export while running".into());
        }
        out.write_all(&self.volume(volume).unwrap_or_default())?;
        Ok(())
    }

    fn import_volume(&self, volume: &str, _image: &str, tar: &mut dyn Read) -> Result<()> {
        self.record(format!("import {volume}"));
        let mut buf = Vec::new();
        tar.read_to_end(&mut buf)?;
        self.volumes.lock().unwrap().insert(volume.to_owned(), buf);
        Ok(())
    }

    fn exec(&self, container: &str, cmd: &[&str], stdin: Option<&mut dyn Read>, stdout: Option<&mut dyn Write>) -> Result<i32> {
        self.record(format!("exec {container} {}", cmd.join(" ")));
        let volume = self.containers.lock().unwrap().iter().find(|c| c.info.id == container).map(|c| c.volume.clone()).ok_or_else(|| Error::Docker(format!("no container {container}")))?;
        if cmd.last().is_some_and(|c| c.contains("fail")) {
            return Ok(1);
        }
        if let Some(out) = stdout {
            out.write_all(&[DUMP_PREFIX, &self.volume(&volume).unwrap_or_default()].concat())?;
        }
        if let Some(input) = stdin {
            let mut buf = Vec::new();
            input.read_to_end(&mut buf)?;
            let data = buf.strip_prefix(DUMP_PREFIX).ok_or_else(|| Error::Docker("restore input is not a dump".into()))?;
            self.volumes.lock().unwrap().insert(volume, data.to_vec());
        }
        Ok(0)
    }
}
