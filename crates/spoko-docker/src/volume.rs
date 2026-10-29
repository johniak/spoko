//! Push/pull of volume snapshots. A snapshot is a chunked stream (dedup across versions) plus
//! a record `{kind, manifest, meta}` sealed into the pointer `name_id(kr, "vol"|"vol-dump", name)`.

use std::io::{Seek, SeekFrom};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use spoko_client::api::ApiClient;
use spoko_client::engine::Engine;
use spoko_client::notifier::Notification;
use spoko_client::transfer::chunks::{Manifest, fetch_stream, push_stream};
use spoko_client::transfer::codec::KIND_BLOB;
use spoko_core::config::{Mismatch as OnMismatch, VolumeCfg, VolumeMode};
use spoko_crypto::{Keyring, name_id, open, seal};
use uuid::Uuid;

use crate::api::{ContainerInfo, DockerApi};
use crate::meta::{Mismatch, VolumeMeta, compare, pg_major_from_tar};
use crate::{Error, Result};

/// Everything a volume operation needs on this machine.
pub struct VolumeCtx<'a> {
    pub docker: &'a dyn DockerApi,
    pub api: &'a ApiClient,
    pub keyring: &'a Keyring,
    pub project: Uuid,
    /// Compose project name here (usually the directory name) for volume name resolution.
    pub project_hint: &'a str,
    pub device_id: Uuid,
    pub device_name: &'a str,
    pub now: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Raw,
    Dump,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub kind: Kind,
    pub manifest: Manifest,
    pub meta: VolumeMeta,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushReport {
    pub meta: VolumeMeta,
    /// New chunks uploaded (the rest were already on the server).
    pub uploaded_chunks: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullOutcome {
    Restored(VolumeMeta),
    /// Nothing changed; the caller asks the user and retries with `force`.
    MismatchWarned(Vec<Mismatch>),
    NoSnapshot,
}

fn pointer_name(kr: &Keyring, kind: Kind, volume: &str) -> String {
    name_id(kr, if kind == Kind::Raw { "vol" } else { "vol-dump" }, volume)
}

fn read_record(ctx: &VolumeCtx<'_>, kind: Kind, volume: &str) -> Result<Option<(VolumeRecord, u64)>> {
    let Some(p) = ctx.api.get_pointer(ctx.project, &pointer_name(ctx.keyring, kind, volume))? else { return Ok(None) };
    let rec = serde_json::from_slice(&open(ctx.keyring, b"volume", &p.value)?).map_err(|e| Error::Other(format!("bad volume record: {e}")))?;
    Ok(Some((rec, p.version)))
}

fn write_record(ctx: &VolumeCtx<'_>, volume: &str, rec: &VolumeRecord) -> Result<()> {
    let value = seal(ctx.keyring, b"volume", &serde_json::to_vec(rec).map_err(|e| Error::Other(e.to_string()))?);
    ctx.api.put_pointer(ctx.project, &pointer_name(ctx.keyring, rec.kind, volume), None, &value)?.map_err(|_| Error::Other("volume pointer".into()))?;
    Ok(())
}

fn owner(users: &[ContainerInfo], volume: &str) -> Result<ContainerInfo> {
    users.first().cloned().ok_or_else(|| Error::Other(format!("no container uses volume {volume} (start it once with docker compose)")))
}

fn running(users: &[ContainerInfo], volume: &str) -> Result<ContainerInfo> {
    users.iter().find(|c| c.running).cloned().ok_or_else(|| Error::Other(format!("dump/restore of {volume} needs a running container")))
}

fn upload(ctx: &VolumeCtx<'_>, file: &mut std::fs::File) -> Result<(Manifest, usize)> {
    file.seek(SeekFrom::Start(0))?;
    Ok(push_stream(&ctx.api.objects(ctx.project), ctx.keyring, &mut *file, KIND_BLOB)?)
}

fn shell(cmd: &str) -> [&str; 3] {
    ["sh", "-c", cmd]
}

/// Snapshot of a volume → server. Raw mode stops the containers using it for the copy.
/// With `on_mismatch = "dump"` and a dump command a dump is pushed too (fallback for the other side).
pub fn push(ctx: &VolumeCtx<'_>, cfg: &VolumeCfg) -> Result<PushReport> {
    let vol = ctx.docker.resolve_volume(&cfg.name, ctx.project_hint)?;
    let users = ctx.docker.containers_using(&vol)?;
    let own = owner(&users, &vol)?;
    let img = ctx.docker.image_info(&own.image_id)?;
    let mut meta = VolumeMeta { image_tags: img.tags, image_id: own.image_id.clone(), arch: img.arch, os: img.os, pg_major: None, created_at: ctx.now, device_name: ctx.device_name.to_owned() };
    let mut uploaded = 0;
    let want_dump = cfg.mode == VolumeMode::Dump || (cfg.on_mismatch == OnMismatch::Dump && cfg.dump.is_some());
    if want_dump {
        let cmd = cfg.dump.as_deref().ok_or_else(|| Error::Other(format!("volume {}: mode dump needs `dump`", cfg.name)))?;
        let c = running(&users, &vol)?;
        let mut tmp = tempfile::tempfile()?;
        let code = ctx.docker.exec(&c.id, &shell(cmd), None, Some(&mut tmp))?;
        if code != 0 {
            return Err(Error::Other(format!("dump of {} failed (exit {code})", cfg.name)));
        }
        let (manifest, n) = upload(ctx, &mut tmp)?;
        uploaded += n;
        write_record(ctx, &cfg.name, &VolumeRecord { kind: Kind::Dump, manifest, meta: meta.clone() })?;
    }
    if cfg.mode == VolumeMode::Raw {
        let stopped: Vec<String> = if cfg.stop_container { users.iter().filter(|c| c.running).map(|c| c.id.clone()).collect() } else { vec![] };
        for id in &stopped {
            ctx.docker.stop(id)?;
        }
        let mut tmp = tempfile::tempfile()?;
        let exported = ctx.docker.export_volume(&vol, &own.image, &mut tmp);
        for id in &stopped {
            ctx.docker.start(id)?;
        }
        exported?;
        tmp.seek(SeekFrom::Start(0))?;
        meta.pg_major = pg_major_from_tar(&mut tmp);
        let (manifest, n) = upload(ctx, &mut tmp)?;
        uploaded += n;
        write_record(ctx, &cfg.name, &VolumeRecord { kind: Kind::Raw, manifest, meta: meta.clone() })?;
    }
    Ok(PushReport { meta, uploaded_chunks: uploaded })
}

fn restore_dump(ctx: &VolumeCtx<'_>, cfg: &VolumeCfg, users: &[ContainerInfo], vol: &str, rec: VolumeRecord) -> Result<PullOutcome> {
    let cmd = cfg.restore.as_deref().ok_or_else(|| Error::Other(format!("volume {}: restoring a dump needs `restore`", cfg.name)))?;
    let c = running(users, vol)?;
    let mut tmp = tempfile::tempfile()?;
    fetch_stream(&ctx.api.objects(ctx.project), ctx.keyring, &rec.manifest, &mut tmp)?;
    tmp.seek(SeekFrom::Start(0))?;
    let code = ctx.docker.exec(&c.id, &shell(cmd), Some(&mut tmp), None)?;
    if code != 0 {
        return Err(Error::Other(format!("restore of {} failed (exit {code})", cfg.name)));
    }
    Ok(PullOutcome::Restored(rec.meta))
}

/// Latest snapshot → local volume. Incompatible snapshots are not applied unless `force`
/// (or a dump fallback exists and `on_mismatch = "dump"`).
pub fn pull(ctx: &VolumeCtx<'_>, cfg: &VolumeCfg, force: bool) -> Result<PullOutcome> {
    let raw = read_record(ctx, Kind::Raw, &cfg.name)?.map(|r| r.0);
    let dump = read_record(ctx, Kind::Dump, &cfg.name)?.map(|r| r.0);
    let vol = match ctx.docker.resolve_volume(&cfg.name, ctx.project_hint) {
        Ok(v) => v,
        // not created here yet: use compose's naming so `docker compose up` picks it up
        Err(Error::NotFound(_)) => format!("{}_{}", ctx.project_hint, cfg.name),
        Err(e) => return Err(e),
    };
    let users = ctx.docker.containers_using(&vol)?;
    if cfg.mode == VolumeMode::Dump {
        return match (dump, raw) {
            (Some(rec), _) => restore_dump(ctx, cfg, &users, &vol, rec),
            (None, Some(_)) => Ok(PullOutcome::MismatchWarned(vec![Mismatch::Mode])),
            (None, None) => Ok(PullOutcome::NoSnapshot),
        };
    }
    let Some(rec) = raw else {
        return Ok(if dump.is_some() { PullOutcome::MismatchWarned(vec![Mismatch::Mode]) } else { PullOutcome::NoSnapshot });
    };
    let local = users.first().map(|c| ctx.docker.image_info(&c.image_id)).transpose()?;
    let mismatches = local.map(|i| compare(&i, &rec.meta)).unwrap_or_default();
    if !mismatches.is_empty() && !force {
        if let (OnMismatch::Dump, Some(d), Some(_)) = (cfg.on_mismatch, dump, cfg.restore.as_ref()) {
            return restore_dump(ctx, cfg, &users, &vol, d);
        }
        return Ok(PullOutcome::MismatchWarned(mismatches));
    }
    let image = users.first().map(|c| c.image.clone()).or_else(|| rec.meta.image_tags.first().cloned()).unwrap_or_else(|| rec.meta.image_id.clone());
    let stopped: Vec<String> = users.iter().filter(|c| c.running).map(|c| c.id.clone()).collect();
    let mut tmp = tempfile::tempfile()?;
    fetch_stream(&ctx.api.objects(ctx.project), ctx.keyring, &rec.manifest, &mut tmp)?;
    tmp.seek(SeekFrom::Start(0))?;
    for id in &stopped {
        ctx.docker.stop(id)?;
    }
    let imported = ctx.docker.import_volume(&vol, &image, &mut tmp);
    for id in &stopped {
        ctx.docker.start(id)?;
    }
    imported?;
    Ok(PullOutcome::Restored(rec.meta))
}

#[derive(Serialize, Deserialize)]
struct Request {
    #[serde(rename = "type")]
    kind: String,
    name_id: String,
}

/// Asks the other devices for a fresh snapshot of `volume` (the daemon there answers with `push`).
pub fn request_fresh(api: &ApiClient, kr: &Keyring, project: Uuid, volume: &str) -> Result<()> {
    let body = serde_json::to_vec(&Request { kind: "volume_request".into(), name_id: name_id(kr, "vol", volume) }).map_err(|e| Error::Other(e.to_string()))?;
    api.post_message(project, &seal(kr, b"message", &body))?;
    Ok(())
}

/// The volume (from `volumes`) a received message asks for, if it is a volume request.
pub fn requested_volume<'a>(kr: &Keyring, payload: &[u8], volumes: &'a [VolumeCfg]) -> Option<&'a VolumeCfg> {
    let req: Request = serde_json::from_slice(&open(kr, b"message", payload).ok()?).ok()?;
    (req.kind == "volume_request").then(|| volumes.iter().find(|v| name_id(kr, "vol", &v.name) == req.name_id)).flatten()
}

/// `spoko volume pull --fresh`: if another device is online, ask it for a new snapshot and wait
/// (up to `timeout`) for the pointer to move; then pull whatever is newest.
pub fn pull_fresh(ctx: &VolumeCtx<'_>, cfg: &VolumeCfg, timeout: Duration, poll: Duration) -> Result<PullOutcome> {
    let kind = if cfg.mode == VolumeMode::Dump { Kind::Dump } else { Kind::Raw };
    let before = read_record(ctx, kind, &cfg.name)?.map_or(0, |r| r.1);
    let online = ctx.api.presence(ctx.project)?.online_devices.into_iter().any(|d| d != ctx.device_id);
    if online {
        request_fresh(ctx.api, ctx.keyring, ctx.project, &cfg.name)?;
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && read_record(ctx, kind, &cfg.name)?.map_or(0, |r| r.1) <= before {
            std::thread::sleep(poll);
        }
    }
    pull(ctx, cfg, false)
}

/// Called on local activity: if another device was the last one active, prompt once per
/// switch for every configured volume ("pull pg_data from A?").
pub fn switch_detected(e: &mut Engine) -> Result<bool> {
    let Some((device, at, version)) = e.last_activity()? else { return Ok(false) };
    if device == e.ctx.device_id || e.state.switch_prompt_version == version {
        return Ok(false);
    }
    e.state.switch_prompt_version = version;
    e.save()?;
    let from_device = e.ctx.api.devices()?.into_iter().find(|d| d.id == device).map_or_else(|| device.to_string(), |d| d.name);
    for v in &e.config().docker.volume {
        e.ctx.notifier.notify(Notification::VolumePrompt { project: e.project_id, volume: v.name.clone(), from_device: from_device.clone(), at });
    }
    Ok(true)
}
