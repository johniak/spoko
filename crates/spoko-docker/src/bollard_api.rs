//! [`DockerApi`] over the real Docker Engine API (bollard), on a private current-thread runtime.

use std::collections::HashMap;
use std::io::{Read, Write};

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::StartExecResults;
use bollard::models::{ContainerCreateBody, ContainerSummaryStateEnum, ExecConfig, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptions, DownloadFromContainerOptions, ListContainersOptions, ListVolumesOptions, RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
    UploadToContainerOptions, WaitContainerOptions,
};
use futures_util::StreamExt;

use crate::api::{ContainerInfo, DockerApi, ImageInfo, pick_volume};
use crate::{Error, Result};

pub struct BollardApi {
    docker: Docker,
    rt: tokio::runtime::Runtime,
}

const MOUNT: &str = "/spoko-vol";
const STDIN_FILE: &str = "/tmp/.spoko-stdin";

fn derr(e: bollard::errors::Error) -> Error {
    Error::Docker(e.to_string())
}

impl BollardApi {
    pub fn connect() -> Result<BollardApi> {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let docker = Docker::connect_with_local_defaults().map_err(derr)?;
        Ok(BollardApi { docker, rt })
    }

    /// A stopped helper container with `volume` mounted at `/spoko-vol`.
    async fn helper(&self, volume: &str, image: &str, cmd: Option<Vec<String>>) -> Result<String> {
        let body = ContainerCreateBody {
            image: Some(image.to_owned()),
            entrypoint: cmd,
            user: Some("0".into()),
            labels: Some(HashMap::from([("dev.spoko.helper".to_owned(), "1".to_owned())])),
            host_config: Some(HostConfig { binds: Some(vec![format!("{volume}:{MOUNT}")]), ..Default::default() }),
            ..Default::default()
        };
        Ok(self.docker.create_container(None::<CreateContainerOptions>, body).await.map_err(derr)?.id)
    }

    async fn remove(&self, id: &str) {
        let _ = self.docker.remove_container(id, Some(RemoveContainerOptions { force: true, ..Default::default() })).await;
    }
}

impl DockerApi for BollardApi {
    fn resolve_volume(&self, logical: &str, project_hint: &str) -> Result<String> {
        let vols = self.rt.block_on(self.docker.list_volumes(None::<ListVolumesOptions>)).map_err(derr)?.volumes.unwrap_or_default();
        let pairs: Vec<(String, HashMap<String, String>)> = vols.into_iter().map(|v| (v.name, v.labels)).collect();
        pick_volume(logical, project_hint, &pairs)
    }

    fn containers_using(&self, volume: &str) -> Result<Vec<ContainerInfo>> {
        let opts = ListContainersOptions { all: true, filters: Some(HashMap::from([("volume".to_owned(), vec![volume.to_owned()])])), ..Default::default() };
        let list = self.rt.block_on(self.docker.list_containers(Some(opts))).map_err(derr)?;
        Ok(list
            .into_iter()
            .filter(|c| !c.labels.as_ref().is_some_and(|l| l.contains_key("dev.spoko.helper")))
            .map(|c| ContainerInfo {
                id: c.id.unwrap_or_default(),
                image: c.image.unwrap_or_default(),
                image_id: c.image_id.unwrap_or_default(),
                running: c.state == Some(ContainerSummaryStateEnum::RUNNING),
            })
            .collect())
    }

    fn image_info(&self, image_id: &str) -> Result<ImageInfo> {
        let i = self.rt.block_on(self.docker.inspect_image(image_id)).map_err(derr)?;
        Ok(ImageInfo { arch: i.architecture.unwrap_or_default(), os: i.os.unwrap_or_default(), tags: i.repo_tags.unwrap_or_default() })
    }

    fn stop(&self, id: &str) -> Result<()> {
        self.rt.block_on(self.docker.stop_container(id, None::<StopContainerOptions>)).map_err(derr)
    }

    fn start(&self, id: &str) -> Result<()> {
        self.rt.block_on(self.docker.start_container(id, None::<StartContainerOptions>)).map_err(derr)
    }

    fn export_volume(&self, volume: &str, image: &str, out: &mut dyn Write) -> Result<()> {
        self.rt.block_on(async {
            let id = self.helper(volume, image, None).await?;
            let mut stream = self.docker.download_from_container(&id, Some(DownloadFromContainerOptions { path: MOUNT.into() }));
            let mut res = Ok(());
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(b) => {
                        if let Err(e) = out.write_all(&b) {
                            res = Err(e.into());
                            break;
                        }
                    }
                    Err(e) => {
                        res = Err(derr(e));
                        break;
                    }
                }
            }
            self.remove(&id).await;
            res
        })
    }

    fn import_volume(&self, volume: &str, image: &str, tar: &mut dyn Read) -> Result<()> {
        let mut data = Vec::new();
        tar.read_to_end(&mut data)?;
        self.rt.block_on(async {
            let wipe = vec!["sh".to_owned(), "-c".to_owned(), format!("find {MOUNT} -mindepth 1 -delete")];
            let id = self.helper(volume, image, Some(wipe)).await?;
            let res = async {
                self.docker.start_container(&id, None::<StartContainerOptions>).await.map_err(derr)?;
                let mut wait = self.docker.wait_container(&id, None::<WaitContainerOptions>);
                while let Some(w) = wait.next().await {
                    w.map_err(derr)?;
                }
                // keep the tar's owners (postgres data must stay owned by its uid)
                let opts = UploadToContainerOptions { path: "/".into(), copy_uidgid: Some("1".into()), ..Default::default() };
                self.docker.upload_to_container(&id, Some(opts), bollard::body_full(data.into())).await.map_err(derr)
            }
            .await;
            self.remove(&id).await;
            res
        })
    }

    fn exec(&self, container: &str, cmd: &[&str], stdin: Option<&mut dyn Read>, stdout: Option<&mut dyn Write>) -> Result<i32> {
        self.rt.block_on(async {
            // stdin is staged as a file in the container and redirected: closing an attached
            // exec stdin is not reliable across Docker versions
            let mut argv: Vec<String> = cmd.iter().map(|s| (*s).to_owned()).collect();
            if let Some(r) = stdin {
                let ["sh", "-c", script] = cmd else { return Err(Error::Other("stdin needs a `sh -c` command".into())) };
                let mut data = Vec::new();
                r.read_to_end(&mut data)?;
                let mut b = tar::Builder::new(Vec::new());
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, STDIN_FILE.trim_start_matches('/'), data.as_slice())?;
                let opts = UploadToContainerOptions { path: "/".into(), ..Default::default() };
                self.docker.upload_to_container(container, Some(opts), bollard::body_full(b.into_inner()?.into())).await.map_err(derr)?;
                argv = vec!["sh".into(), "-c".into(), format!("({script}) < {STDIN_FILE}; rc=$?; rm -f {STDIN_FILE}; exit $rc")];
            }
            let cfg = ExecConfig { attach_stdout: Some(true), attach_stderr: Some(true), cmd: Some(argv), ..Default::default() };
            let ex = self.docker.create_exec(container, cfg).await.map_err(derr)?;
            if let StartExecResults::Attached { mut output, .. } = self.docker.start_exec(&ex.id, None).await.map_err(derr)? {
                let mut out = stdout;
                while let Some(item) = output.next().await {
                    if let (LogOutput::StdOut { message }, Some(w)) = (item.map_err(derr)?, out.as_mut()) {
                        w.write_all(&message)?;
                    }
                }
            }
            let info = self.docker.inspect_exec(&ex.id).await.map_err(derr)?;
            Ok(i32::try_from(info.exit_code.unwrap_or(-1)).unwrap_or(-1))
        })
    }
}
