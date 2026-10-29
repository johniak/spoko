use std::collections::HashMap;

use spoko_client::account::{self, Identity};
use spoko_client::api::ApiClient;
use spoko_core::config::{Mismatch as OnMismatch, VolumeCfg, VolumeMode};
use spoko_docker::Error;
use spoko_docker::api::{COMPOSE_PROJECT_LABEL, COMPOSE_VOLUME_LABEL, DockerApi};
use spoko_docker::fake::FakeDocker;
use spoko_docker::volume::{self, PullOutcome, VolumeCtx};
use spoko_server::test_util::{TestServer, spawn_test_server};
use uuid::Uuid;

struct One {
    _srv: TestServer,
    _home: tempfile::TempDir,
    id: Identity,
    api: ApiClient,
    project: Uuid,
}

fn one() -> One {
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let (id, _) = account::login_bootstrap(home.path(), &srv.url, &srv.bootstrap_token("jan"), "mac", None).unwrap();
    let api = id.api();
    let project = api.create_project(b"m").unwrap().id;
    One { _srv: srv, _home: home, id, api, project }
}

impl One {
    fn ctx<'a>(&'a self, docker: &'a dyn DockerApi, hint: &'a str) -> VolumeCtx<'a> {
        VolumeCtx { docker, api: &self.api, keyring: &self.id.keyring, project: self.project, project_hint: hint, device_id: self.id.device_id, device_name: &self.id.device_name, now: 1 }
    }
}

fn raw(name: &str) -> VolumeCfg {
    VolumeCfg { name: name.into(), mode: VolumeMode::Raw, stop_container: true, dump: None, restore: None, on_mismatch: OnMismatch::Warn }
}

#[test]
fn pull_with_an_ambiguous_volume_name_fails_listing_the_candidates() {
    let t = one();
    let d = FakeDocker::default();
    for project in ["dir1", "dir2"] {
        let name = format!("{project}_pgdata");
        d.volumes.lock().unwrap().insert(name.clone(), b"x".to_vec());
        d.labels.lock().unwrap().insert(name, HashMap::from([(COMPOSE_VOLUME_LABEL.to_owned(), "pgdata".to_owned()), (COMPOSE_PROJECT_LABEL.to_owned(), project.to_owned())]));
    }
    let Err(Error::Ambiguous { logical, mut candidates }) = volume::pull(&t.ctx(&d, "elsewhere"), &raw("pgdata"), false) else { panic!("ambiguous name must fail") };
    candidates.sort();
    assert_eq!(logical, "pgdata");
    assert_eq!(candidates, vec!["dir1_pgdata", "dir2_pgdata"]);
    assert!(d.take_log().is_empty(), "nothing was stopped or imported");
}

#[test]
fn dump_mode_pull_without_any_snapshot_reports_no_snapshot() {
    let t = one();
    let d = FakeDocker::with_volume("p", "pg", b"local", "postgres:16", "amd64");
    let cfg = VolumeCfg { mode: VolumeMode::Dump, dump: Some("pg_dump".into()), restore: Some("pg_restore".into()), ..raw("pg") };
    assert_eq!(volume::pull(&t.ctx(&d, "p"), &cfg, false).unwrap(), PullOutcome::NoSnapshot);
    assert_eq!(d.volume("p_pg").unwrap(), b"local");
    assert!(d.take_log().is_empty());
}

#[test]
fn pull_of_a_snapshot_record_that_does_not_decrypt_is_a_crypto_error() {
    let t = one();
    let d = FakeDocker::with_volume("p", "pg", b"local", "postgres:16", "amd64");
    let name = spoko_crypto::name_id(&t.id.keyring, "vol", "pg");
    t.api.put_pointer(t.project, &name, None, b"not sealed with this keyring").unwrap().unwrap();
    let err = volume::pull(&t.ctx(&d, "p"), &raw("pg"), false).unwrap_err();
    assert!(matches!(err, Error::Client(spoko_client::ClientError::Crypto(_))), "{err:?}");
    assert_eq!(d.volume("p_pg").unwrap(), b"local", "nothing was restored");
}

#[test]
fn snapshot_record_that_is_not_a_volume_record_is_reported_as_bad() {
    let t = one();
    let d = FakeDocker::with_volume("p", "pg", b"local", "postgres:16", "amd64");
    let name = spoko_crypto::name_id(&t.id.keyring, "vol", "pg");
    t.api.put_pointer(t.project, &name, None, &spoko_crypto::seal(&t.id.keyring, b"volume", b"{\"kind\":\"tape\"}")).unwrap().unwrap();
    let err = volume::pull(&t.ctx(&d, "p"), &raw("pg"), false).unwrap_err();
    assert!(err.to_string().starts_with("bad volume record: "), "{err}");
}

#[test]
fn push_of_a_volume_no_container_uses_asks_to_start_one() {
    let t = one();
    let d = FakeDocker::default();
    d.volumes.lock().unwrap().insert("orphan".into(), b"x".to_vec());
    let err = volume::push(&t.ctx(&d, "p"), &raw("orphan")).unwrap_err();
    assert_eq!(err.to_string(), "no container uses volume orphan (start it once with docker compose)");
}

#[test]
fn restoring_a_dump_without_a_restore_command_fails_and_changes_nothing() {
    let t = one();
    let d = FakeDocker::with_volume("p", "pg", b"rows", "postgres:16", "amd64");
    let cfg = VolumeCfg { mode: VolumeMode::Dump, dump: Some("pg_dump".into()), ..raw("pg") };
    volume::push(&t.ctx(&d, "p"), &cfg).unwrap();
    d.volumes.lock().unwrap().insert("p_pg".into(), b"newer local rows".to_vec());
    d.take_log();
    let err = volume::pull(&t.ctx(&d, "p"), &cfg, false).unwrap_err();
    assert_eq!(err.to_string(), "volume pg: restoring a dump needs `restore`");
    assert_eq!(d.volume("p_pg").unwrap(), b"newer local rows");
    assert!(d.take_log().is_empty());
}

#[test]
fn pull_of_an_untagged_image_snapshot_onto_a_machine_without_containers_restores_it() {
    let t = one();
    let src = FakeDocker::with_volume("p", "pg", b"rows", "postgres:16", "amd64");
    src.images.lock().unwrap().get_mut("sha256:postgres:16").unwrap().tags.clear();
    let rep = volume::push(&t.ctx(&src, "p"), &raw("pg")).unwrap();
    assert!(rep.meta.image_tags.is_empty());
    let dst = FakeDocker::default();
    let PullOutcome::Restored(meta) = volume::pull(&t.ctx(&dst, "q"), &raw("pg"), false).unwrap() else { panic!("restored") };
    assert_eq!(meta.image_id, "sha256:postgres:16");
    assert_eq!(dst.volume("q_pg").unwrap(), b"rows");
    assert_eq!(dst.take_log(), vec!["import q_pg"]);
}

#[test]
fn fake_restore_rejects_input_that_is_not_a_dump() {
    let d = FakeDocker::with_volume("p", "pg", b"rows", "postgres:16", "amd64");
    let err = d.exec("p-db", &["sh", "-c", "pg_restore"], Some(&mut &b"raw bytes"[..]), None).unwrap_err();
    assert_eq!(err.to_string(), "docker: restore input is not a dump");
    assert_eq!(d.volume("p_pg").unwrap(), b"rows");
}

#[derive(Default)]
struct Notes(std::sync::Mutex<Vec<spoko_client::notifier::Notification>>);

impl spoko_client::notifier::Notifier for Notes {
    fn notify(&self, n: spoko_client::notifier::Notification) {
        self.0.lock().unwrap().push(n);
    }
}

#[test]
fn device_switch_from_a_device_the_account_does_not_list_names_it_by_id() {
    let t = one();
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("app");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join(".spokoconfig"), "[[docker.volume]]\nname = \"pgdata\"\n").unwrap();
    let notes = std::sync::Arc::new(Notes::default());
    let env = spoko_core::external::ExternalEnv { home: d.path().join("h"), claude_config_dir: Some(d.path().join("c")) };
    let ctx = spoko_client::engine::DeviceCtx::new(&d.path().join("home"), &t.id, env, notes.clone());
    let mut e = spoko_client::engine::Engine::create(ctx, &root, "p").unwrap();
    let stranger = Uuid::new_v4();
    let body = serde_json::json!({"device_id": stranger, "time": 42}).to_string();
    let kr = &t.id.keyring;
    t.api.put_pointer(e.project_id, &spoko_crypto::name_id(kr, "ptr", "activity"), None, &spoko_crypto::seal(kr, b"activity", body.as_bytes())).unwrap().unwrap();
    notes.0.lock().unwrap().clear();
    assert!(volume::switch_detected(&mut e).unwrap());
    let seen = std::mem::take(&mut *notes.0.lock().unwrap());
    assert!(matches!(&seen[..], [spoko_client::notifier::Notification::VolumePrompt { volume, from_device, at: 42, .. }] if volume == "pgdata" && *from_device == stranger.to_string()), "{seen:?}");
}
