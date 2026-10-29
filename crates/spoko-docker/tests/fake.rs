use std::sync::Arc;
use std::time::Duration;

use rand::RngCore;
use spoko_client::account::{self, Identity};
use spoko_client::engine::{DeviceCtx, Engine};
use spoko_client::notifier::{Notification, RecordingNotifier};
use spoko_client::ws::EventStream;
use spoko_core::config::{Mismatch as OnMismatch, VolumeCfg, VolumeMode};
use spoko_core::external::ExternalEnv;
use spoko_docker::api::{COMPOSE_PROJECT_LABEL, COMPOSE_VOLUME_LABEL, DockerApi};
use spoko_docker::fake::FakeDocker;
use spoko_docker::meta::{Mismatch, pg_major_from_tar};
use spoko_docker::volume::{self, PullOutcome, VolumeCtx};
use spoko_proto::ServerEvent;
use spoko_server::test_util::{TestServer, spawn_test_server};
use uuid::Uuid;

struct Two {
    srv: TestServer,
    a: Identity,
    b: Identity,
    project: Uuid,
    _homes: Vec<tempfile::TempDir>,
}

fn two() -> Two {
    let srv = spawn_test_server();
    let (ha, hb) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, rk) = account::login_bootstrap(ha.path(), &srv.url, &srv.bootstrap_token("jan"), "mac", None).unwrap();
    let (b, _) = account::login_bootstrap(hb.path(), &srv.url, &srv.bootstrap_token("jan"), "linux", rk.as_ref()).unwrap();
    let project = a.api().create_project(b"m").unwrap().id;
    Two { srv, a, b, project, _homes: vec![ha, hb] }
}

fn ctx<'a>(id: &'a Identity, api: &'a spoko_client::api::ApiClient, docker: &'a dyn DockerApi, project: Uuid, hint: &'a str) -> VolumeCtx<'a> {
    VolumeCtx { docker, api, keyring: &id.keyring, project, project_hint: hint, device_id: id.device_id, device_name: &id.device_name, now: 1 }
}

fn raw(name: &str) -> VolumeCfg {
    VolumeCfg { name: name.into(), mode: VolumeMode::Raw, stop_container: true, dump: None, restore: None, on_mismatch: OnMismatch::Warn }
}

fn pg_tar(version: &str, extra: &[u8]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, data) in [("spoko-vol/PG_VERSION", format!("{version}\n").into_bytes()), ("spoko-vol/base/1", extra.to_vec())] {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o600);
        h.set_cksum();
        b.append_data(&mut h, path, data.as_slice()).unwrap();
    }
    b.into_inner().unwrap()
}

#[test]
fn raw_push_pull_roundtrip_stops_and_starts() {
    let t = two();
    let data = pg_tar("16", b"rows");
    let da = FakeDocker::with_volume("projA", "pgdata", &data, "postgres:16", "arm64");
    let db = FakeDocker::with_volume("projB", "pgdata", b"", "postgres:16", "arm64");
    let (api_a, api_b) = (t.a.api(), t.b.api());
    let rep = volume::push(&ctx(&t.a, &api_a, &da, t.project, "projA"), &raw("pgdata")).unwrap();
    assert_eq!(rep.meta.pg_major.as_deref(), Some("16"));
    assert_eq!(rep.meta.device_name, "mac");
    assert_eq!(da.take_log(), vec!["stop projA-db", "export projA_pgdata", "start projA-db"]);
    let out = volume::pull(&ctx(&t.b, &api_b, &db, t.project, "projB"), &raw("pgdata"), false).unwrap();
    assert!(matches!(out, PullOutcome::Restored(m) if m.arch == "arm64"));
    assert_eq!(db.volume("projB_pgdata").unwrap(), data);
    assert_eq!(db.take_log(), vec!["stop projB-db", "import projB_pgdata", "start projB-db"]);
    // no snapshot of another volume
    assert_eq!(volume::pull(&ctx(&t.b, &api_b, &db, t.project, "projB"), &raw("redis"), false).unwrap(), PullOutcome::NoSnapshot);
}

#[test]
fn pull_into_machine_without_the_volume_uses_compose_name() {
    let t = two();
    let da = FakeDocker::with_volume("projA", "pgdata", b"data", "postgres:16", "arm64");
    let db = FakeDocker::default();
    let (api_a, api_b) = (t.a.api(), t.b.api());
    volume::push(&ctx(&t.a, &api_a, &da, t.project, "projA"), &raw("pgdata")).unwrap();
    assert!(matches!(volume::pull(&ctx(&t.b, &api_b, &db, t.project, "projB"), &raw("pgdata"), false).unwrap(), PullOutcome::Restored(_)));
    assert_eq!(db.volume("projB_pgdata").unwrap(), b"data");
    // push without any container using the volume is an error
    assert!(volume::push(&ctx(&t.b, &api_b, &db, t.project, "projB"), &raw("pgdata")).is_err());
}

#[test]
fn stop_container_false_exports_live() {
    let t = two();
    let da = FakeDocker::with_volume("p", "v", b"x", "redis:7", "amd64");
    let api = t.a.api();
    let cfg = VolumeCfg { stop_container: false, ..raw("v") };
    volume::push(&ctx(&t.a, &api, &da, t.project, "p"), &cfg).unwrap();
    assert_eq!(da.take_log(), vec!["export p_v", "export while running"]);
}

#[test]
fn dump_mode_uses_exec() {
    let t = two();
    let da = FakeDocker::with_volume("p", "pg", b"server rows", "postgres:16", "amd64");
    let db = FakeDocker::with_volume("q", "pg", b"old rows", "postgres:16", "amd64");
    let (api_a, api_b) = (t.a.api(), t.b.api());
    let cfg = VolumeCfg { mode: VolumeMode::Dump, dump: Some("pg_dump -Fc".into()), restore: Some("pg_restore --clean".into()), ..raw("pg") };
    volume::push(&ctx(&t.a, &api_a, &da, t.project, "p"), &cfg).unwrap();
    assert_eq!(da.take_log(), vec!["exec p-db sh -c pg_dump -Fc"], "dump never stops the container");
    assert!(matches!(volume::pull(&ctx(&t.b, &api_b, &db, t.project, "q"), &cfg, false).unwrap(), PullOutcome::Restored(_)));
    assert_eq!(db.volume("q_pg").unwrap(), b"server rows");
    assert_eq!(db.take_log(), vec!["exec q-db sh -c pg_restore --clean"]);
    // raw config but only a dump on the server → mode mismatch
    assert_eq!(volume::pull(&ctx(&t.b, &api_b, &db, t.project, "q"), &raw("pg"), false).unwrap(), PullOutcome::MismatchWarned(vec![Mismatch::Mode]));
    // failing dump
    let bad = VolumeCfg { dump: Some("fail".into()), ..cfg.clone() };
    assert!(volume::push(&ctx(&t.a, &api_a, &da, t.project, "p"), &bad).is_err());
    let missing = VolumeCfg { dump: None, ..cfg.clone() };
    assert!(volume::push(&ctx(&t.a, &api_a, &da, t.project, "p"), &missing).is_err());
    let bad_restore = VolumeCfg { restore: Some("fail".into()), ..cfg.clone() };
    assert!(volume::pull(&ctx(&t.b, &api_b, &db, t.project, "q"), &bad_restore, false).is_err());
    // dump config but only raw snapshot
    let t2 = two();
    let (api2a, api2b) = (t2.a.api(), t2.b.api());
    volume::push(&ctx(&t2.a, &api2a, &da, t2.project, "p"), &raw("pg")).unwrap();
    assert_eq!(volume::pull(&ctx(&t2.b, &api2b, &db, t2.project, "q"), &cfg, false).unwrap(), PullOutcome::MismatchWarned(vec![Mismatch::Mode]));
    // dump needs a running container
    db.stop("q-db").unwrap();
    assert!(volume::push(&ctx(&t2.b, &api2b, &db, t2.project, "q"), &cfg).is_err());
}

#[test]
fn mismatch_arch_warns_without_changes() {
    let t = two();
    let da = FakeDocker::with_volume("p", "pg", &pg_tar("16", b"a"), "postgres:16", "amd64");
    let db = FakeDocker::with_volume("q", "pg", b"mine", "postgres:15", "arm64");
    let (api_a, api_b) = (t.a.api(), t.b.api());
    volume::push(&ctx(&t.a, &api_a, &da, t.project, "p"), &raw("pg")).unwrap();
    let PullOutcome::MismatchWarned(ms) = volume::pull(&ctx(&t.b, &api_b, &db, t.project, "q"), &raw("pg"), false).unwrap() else { panic!() };
    assert!(ms.contains(&Mismatch::Arch { local: "arm64".into(), snapshot: "amd64".into() }));
    assert!(ms.contains(&Mismatch::PgMajor { local: "15".into(), snapshot: "16".into() }));
    assert!(ms.iter().any(|m| matches!(m, Mismatch::ImageTag { .. })));
    assert!(ms.iter().all(|m| !m.to_string().is_empty()));
    assert_eq!(db.volume("q_pg").unwrap(), b"mine", "nothing changed");
    assert!(matches!(volume::pull(&ctx(&t.b, &api_b, &db, t.project, "q"), &raw("pg"), true).unwrap(), PullOutcome::Restored(_)));
    assert_eq!(db.volume("q_pg").unwrap(), pg_tar("16", b"a"));
}

#[test]
fn mismatch_falls_back_to_dump_when_configured() {
    let t = two();
    let da = FakeDocker::with_volume("p", "pg", b"rows-a", "postgres:16", "amd64");
    let db = FakeDocker::with_volume("q", "pg", b"rows-b", "postgres:16", "arm64");
    let (api_a, api_b) = (t.a.api(), t.b.api());
    let cfg = VolumeCfg { on_mismatch: OnMismatch::Dump, dump: Some("pg_dump".into()), restore: Some("pg_restore".into()), ..raw("pg") };
    volume::push(&ctx(&t.a, &api_a, &da, t.project, "p"), &cfg).unwrap();
    assert_eq!(da.take_log(), vec!["exec p-db sh -c pg_dump", "stop p-db", "export p_pg", "start p-db"], "raw and dump both pushed");
    assert!(matches!(volume::pull(&ctx(&t.b, &api_b, &db, t.project, "q"), &cfg, false).unwrap(), PullOutcome::Restored(_)));
    assert_eq!(db.take_log(), vec!["exec q-db sh -c pg_restore"]);
    assert_eq!(db.volume("q_pg").unwrap(), b"rows-a");
}

#[test]
fn pg_major_detected_from_tar() {
    assert_eq!(pg_major_from_tar(&pg_tar("16", b"")[..]).as_deref(), Some("16"));
    assert_eq!(pg_major_from_tar(&b"not a tar at all"[..]), None);
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_size(3);
    h.set_cksum();
    b.append_data(&mut h, "spoko-vol/a/b/c/d/PG_VERSION", &b"13\n"[..]).unwrap();
    assert_eq!(pg_major_from_tar(&b.into_inner().unwrap()[..]), None, "too deep");
}

#[test]
fn resolve_compose_label() {
    let d = FakeDocker::default();
    let lbl = |logical: &str, project: &str| std::collections::HashMap::from([(COMPOSE_VOLUME_LABEL.to_owned(), logical.to_owned()), (COMPOSE_PROJECT_LABEL.to_owned(), project.to_owned())]);
    d.volumes.lock().unwrap().insert("exact".into(), vec![]);
    d.volumes.lock().unwrap().insert("dir1_pgdata".into(), vec![]);
    d.labels.lock().unwrap().insert("dir1_pgdata".into(), lbl("pgdata", "dir1"));
    assert_eq!(d.resolve_volume("exact", "x").unwrap(), "exact");
    assert_eq!(d.resolve_volume("pgdata", "x").unwrap(), "dir1_pgdata");
    d.volumes.lock().unwrap().insert("dir2_pgdata".into(), vec![]);
    d.labels.lock().unwrap().insert("dir2_pgdata".into(), lbl("pgdata", "dir2"));
    assert_eq!(d.resolve_volume("pgdata", "dir2").unwrap(), "dir2_pgdata");
    assert!(matches!(d.resolve_volume("pgdata", "other"), Err(spoko_docker::Error::Ambiguous { .. })));
    assert!(matches!(d.resolve_volume("nope", "x"), Err(spoko_docker::Error::NotFound(_))));
    assert!(d.image_info("missing").is_err());
    assert!(d.stop("missing").is_err());
    assert!(d.exec("missing", &["x"], None, None).is_err());
}

#[test]
fn incremental_second_push_small() {
    let t = two();
    let mut big = vec![0u8; 50 << 20];
    rand::thread_rng().fill_bytes(&mut big);
    let da = FakeDocker::with_volume("p", "pg", &big, "postgres:16", "amd64");
    let api = t.a.api();
    let first = volume::push(&ctx(&t.a, &api, &da, t.project, "p"), &raw("pg")).unwrap();
    assert!(first.uploaded_chunks >= 20, "{first:?}");
    big[25 << 20] ^= 0xff;
    da.volumes.lock().unwrap().insert("p_pg".into(), big);
    let second = volume::push(&ctx(&t.a, &api, &da, t.project, "p"), &raw("pg")).unwrap();
    assert!(second.uploaded_chunks <= 3, "{second:?}");
}

fn engine(id: &Identity, home: &std::path::Path, root: &std::path::Path, project: Option<Uuid>) -> (Engine, Arc<RecordingNotifier>) {
    let notes = Arc::new(RecordingNotifier::default());
    let env = ExternalEnv { home: home.join("h"), claude_config_dir: Some(home.join("c")) };
    let ctx = DeviceCtx::new(home, id, env, notes.clone());
    let e = match project {
        None => Engine::create(ctx, root, "p").unwrap(),
        Some(p) => Engine::clone_into(ctx, p, root).unwrap().0,
    };
    (e, notes)
}

#[test]
fn switch_prompt_once() {
    let t = two();
    let d = tempfile::tempdir().unwrap();
    let ra = d.path().join("a");
    std::fs::create_dir_all(&ra).unwrap();
    std::fs::write(ra.join(".spokoconfig"), "[[docker.volume]]\nname = \"pgdata\"\n").unwrap();
    let (ea, _) = engine(&t.a, &d.path().join("ha"), &ra, None);
    let (mut eb, notes) = engine(&t.b, &d.path().join("hb"), &d.path().join("b"), Some(ea.project_id));
    assert!(volume::switch_detected(&mut eb).unwrap());
    let seen = notes.take();
    assert!(matches!(&seen[..], [Notification::VolumePrompt { volume, from_device, .. }] if volume == "pgdata" && from_device == "mac"), "{seen:?}");
    assert!(!volume::switch_detected(&mut eb).unwrap(), "only once per switch");
    let mut ea = ea;
    assert!(!volume::switch_detected(&mut ea).unwrap(), "own activity is no switch");
    drop(t.srv);
}

#[test]
fn request_fresh_waits_for_pointer() {
    let t = two();
    let da = Arc::new(FakeDocker::with_volume("p", "pg", b"v1", "postgres:16", "amd64"));
    let db = FakeDocker::with_volume("q", "pg", b"", "postgres:16", "amd64");
    let api_a = t.a.api();
    volume::push(&ctx(&t.a, &api_a, &*da, t.project, "p"), &raw("pg")).unwrap();
    da.volumes.lock().unwrap().insert("p_pg".into(), b"v2".to_vec());
    // A's daemon: answers a volume request with a push
    let mut ws = EventStream::connect(&t.srv.url, &t.a.token).unwrap();
    assert!(matches!(ws.next(Duration::from_secs(5)).unwrap(), Some(ServerEvent::PresenceChanged { .. })));
    let (kr, project, url, token, dev, da2) = (t.a.keyring.clone(), t.project, t.srv.url.clone(), t.a.token.clone(), t.a.device_id, da.clone());
    let source = std::thread::spawn(move || {
        let api = spoko_client::api::ApiClient::new(&url, Some(token));
        let cfgs = vec![raw("pg"), raw("other")];
        for _ in 0..100 {
            if let Ok(Some(ServerEvent::Message { payload, .. })) = ws.next(Duration::from_millis(100))
                && let Some(cfg) = volume::requested_volume(&kr, &payload, &cfgs)
            {
                let c = VolumeCtx { docker: &*da2, api: &api, keyring: &kr, project, project_hint: "p", device_id: dev, device_name: "mac", now: 2 };
                volume::push(&c, cfg).unwrap();
                return true;
            }
        }
        false
    });
    let api_b = t.b.api();
    let out = volume::pull_fresh(&ctx(&t.b, &api_b, &db, t.project, "q"), &raw("pg"), Duration::from_secs(10), Duration::from_millis(20)).unwrap();
    assert!(source.join().unwrap());
    assert!(matches!(out, PullOutcome::Restored(m) if m.created_at == 2));
    assert_eq!(db.volume("q_pg").unwrap(), b"v2");
    // nobody else online → takes the latest without waiting
    let t2 = std::time::Instant::now();
    volume::pull_fresh(&ctx(&t.b, &api_b, &db, t.project, "q"), &raw("pg"), Duration::from_secs(10), Duration::from_millis(20)).unwrap();
    assert!(t2.elapsed() < Duration::from_secs(5));
    assert!(volume::requested_volume(&t.a.keyring, b"garbage", &[raw("pg")]).is_none());
}
