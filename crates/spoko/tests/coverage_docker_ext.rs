//! Docker volume sync as the daemon and the CLI drive it, against the in-memory Docker.
#![cfg_attr(windows, allow(unused_imports, dead_code))] // helpers of #[cfg(unix)] tests

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use spoko::docker_ext::DockerExt;
use spoko_client::account::{self, Identity};
use spoko_client::daemon::Extensions;
use spoko_client::daemon::ipc::{Request, Response};
use spoko_client::engine::{DeviceCtx, Engine};
use spoko_client::notifier::{Notification, Notifier};
use spoko_core::external::ExternalEnv;
use spoko_docker::api::DockerApi;
use spoko_docker::fake::FakeDocker;
use spoko_server::test_util::{TestServer, spawn_test_server};

const ONE_VOLUME: &str = "[[docker.volume]]\nname = \"pgdata\"\n";

#[derive(Default)]
struct Notes(Mutex<Vec<Notification>>);

impl Notifier for Notes {
    fn notify(&self, n: Notification) {
        self.0.lock().unwrap().push(n);
    }
}

impl Notes {
    fn take(&self) -> Vec<Notification> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// Two logged-in devices ("mac", "linux") of one account.
struct World {
    _srv: TestServer,
    tmp: tempfile::TempDir,
    mac: Identity,
    linux: Identity,
}

fn world() -> World {
    let srv = spawn_test_server();
    let tmp = tempfile::tempdir().unwrap();
    let (mac, rk) = account::login_bootstrap(&tmp.path().join("mac"), &srv.url, &srv.bootstrap_token("jan"), "mac", None).unwrap();
    let (linux, _) = account::login_bootstrap(&tmp.path().join("linux"), &srv.url, &srv.bootstrap_token("jan"), "linux", rk.as_ref()).unwrap();
    World { _srv: srv, tmp, mac, linux }
}

impl World {
    fn path(&self, p: &str) -> PathBuf {
        self.tmp.path().join(p)
    }

    fn ctx(&self, id: &Identity, notes: Arc<Notes>) -> DeviceCtx {
        let home = self.path(&id.device_name);
        let env = ExternalEnv { home: home.join("h"), claude_config_dir: Some(home.join("claude")) };
        DeviceCtx::new(&home, id, env, notes)
    }

    /// A project on "mac" in `root` with the given `.spokoconfig`.
    fn create(&self, root: &Path, config: &str) -> (Engine, Arc<Notes>) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(".spokoconfig"), config).unwrap();
        let notes = Arc::new(Notes::default());
        (Engine::create(self.ctx(&self.mac, notes.clone()), root, "p").unwrap(), notes)
    }

    /// The same project checked out on "linux" in `root`.
    fn clone(&self, project: uuid::Uuid, root: &Path) -> (Engine, Arc<Notes>) {
        let notes = Arc::new(Notes::default());
        let e = Engine::clone_into(self.ctx(&self.linux, notes.clone()), project, root).unwrap().0;
        notes.take();
        (e, notes)
    }
}

fn ext(docker: &Arc<FakeDocker>) -> DockerExt {
    let d = docker.clone();
    DockerExt::with(Arc::new(move || Ok(d.clone() as Arc<dyn DockerApi>)))
}

fn pushed(text: &str) -> bool {
    text.starts_with("pgdata: wysłano (") && text.ends_with(" nowych fragmentów)")
}

#[test]
fn volume_push_then_pull_restores_it_on_the_other_machine_under_the_compose_name() {
    let w = world();
    let (mac, _) = w.create(&w.path("m/src/app"), ONE_VOLUME);
    let mac_docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"rows from mac", "postgres:16", "arm64"));
    let text = ext(&mac_docker).run_volumes(&mac, None, None).unwrap();
    assert!(pushed(&text), "{text}");
    assert_eq!(mac_docker.take_log(), vec!["stop app-db", "export app_pgdata", "start app-db"]);
    // the linux checkout lives in "My App!": compose would call its volume `myapp_pgdata`
    let (linux, _) = w.clone(mac.project_id, &w.path("l/My App!"));
    assert_eq!(spoko::docker_ext::compose_project(&linux), "myapp");
    let linux_docker = Arc::new(FakeDocker::default());
    let text = ext(&linux_docker).run_volumes(&linux, Some("pgdata"), Some((false, false))).unwrap();
    assert!(text.starts_with("pgdata: przywrócono stan z mac ("), "{text}");
    assert_eq!(linux_docker.volume("myapp_pgdata").unwrap(), b"rows from mac");
    // --fresh with no other device online pulls the newest snapshot right away
    linux_docker.volumes.lock().unwrap().insert("myapp_pgdata".into(), b"stale".to_vec());
    let text = ext(&linux_docker).run_volumes(&linux, None, Some((false, true))).unwrap();
    assert!(text.starts_with("pgdata: przywrócono stan z mac ("), "{text}");
    assert_eq!(linux_docker.volume("myapp_pgdata").unwrap(), b"rows from mac");
}

#[test]
fn volume_pull_with_mismatched_image_is_refused_unless_forced() {
    let w = world();
    let (mac, _) = w.create(&w.path("m/app"), ONE_VOLUME);
    let mac_docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"arm rows", "postgres:16", "arm64"));
    ext(&mac_docker).run_volumes(&mac, None, None).unwrap();
    let (linux, _) = w.clone(mac.project_id, &w.path("l/app"));
    let linux_docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"amd rows", "postgres:16", "amd64"));
    let text = ext(&linux_docker).run_volumes(&linux, None, Some((false, false))).unwrap();
    assert_eq!(text, "pgdata: NIE przywrócono — niezgodność: architektura: lokalnie amd64, w snapshocie arm64. Użyj --force, jeśli na pewno.");
    assert_eq!(linux_docker.volume("app_pgdata").unwrap(), b"amd rows", "nothing changed");
    let text = ext(&linux_docker).run_volumes(&linux, None, Some((true, false))).unwrap();
    assert!(text.starts_with("pgdata: przywrócono stan z mac"), "{text}");
    assert_eq!(linux_docker.volume("app_pgdata").unwrap(), b"arm rows");
}

#[test]
fn volume_pull_in_dump_mode_against_a_raw_snapshot_names_the_mode_mismatch() {
    let w = world();
    let root = w.path("m/app");
    let (mut mac, _) = w.create(&root, ONE_VOLUME);
    let docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"rows", "postgres:16", "arm64"));
    ext(&docker).run_volumes(&mac, None, None).unwrap();
    std::fs::write(root.join(".spokoconfig"), "[[docker.volume]]\nname = \"pgdata\"\nmode = \"dump\"\ndump = \"pg_dump\"\nrestore = \"pg_restore\"\n").unwrap();
    mac.sync().unwrap();
    let text = ext(&docker).run_volumes(&mac, None, Some((false, false))).unwrap();
    assert_eq!(text, "pgdata: NIE przywrócono — niezgodność: snapshot jest w innym trybie (raw/dump) niż konfiguracja. Użyj --force, jeśli na pewno.");
}

#[test]
fn volume_pull_without_any_snapshot_says_there_is_none() {
    let w = world();
    let (mac, _) = w.create(&w.path("m/app"), ONE_VOLUME);
    let docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"local rows", "postgres:16", "arm64"));
    let text = ext(&docker).run_volumes(&mac, Some("pgdata"), Some((false, false))).unwrap();
    assert_eq!(text, "pgdata: brak snapshotu na serwerze");
    assert_eq!(docker.volume("app_pgdata").unwrap(), b"local rows");
}

#[test]
fn unknown_volume_fails_before_docker_is_contacted() {
    let w = world();
    let (mac, _) = w.create(&w.path("m/app"), ONE_VOLUME);
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let broken = DockerExt::with(Arc::new(move || {
        c.fetch_add(1, Ordering::SeqCst);
        Err(spoko_docker::Error::Docker("no socket".into()))
    }));
    let err = broken.run_volumes(&mac, Some("redis"), None).unwrap_err();
    assert_eq!(err.to_string(), "brak takich wolumenów w .spokoconfig ([[docker.volume]])");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // a configured volume needs Docker: the connection error is passed on
    assert_eq!(broken.run_volumes(&mac, Some("pgdata"), None).unwrap_err().to_string(), "docker: no socket");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn daemon_volume_requests_are_answered_by_the_extension() {
    let w = world();
    let (mut mac, _) = w.create(&w.path("m/app"), ONE_VOLUME);
    let docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"rows", "postgres:16", "arm64"));
    let x = ext(&docker);
    let project = mac.project_id;
    let Some(Response::Text { text }) = x.handle(&mut mac, &Request::VolumePush { project, volume: None }) else { panic!("push answers with text") };
    assert!(pushed(&text), "{text}");
    let Some(Response::Text { text }) = x.handle(&mut mac, &Request::VolumePull { project, volume: Some("pgdata".into()), force: false, fresh: false }) else { panic!("pull answers with text") };
    assert!(text.starts_with("pgdata: przywrócono stan z mac"), "{text}");
    let r = x.handle(&mut mac, &Request::VolumePull { project, volume: Some("nope".into()), force: true, fresh: false });
    assert_eq!(r, Some(Response::Error { message: "brak takich wolumenów w .spokoconfig ([[docker.volume]])".into() }));
    assert_eq!(x.handle(&mut mac, &Request::Sync { project }), None, "not a volume request");
}

#[test]
fn volume_request_message_from_another_device_pushes_a_fresh_snapshot() {
    let w = world();
    let (mut mac, _) = w.create(&w.path("m/app"), ONE_VOLUME);
    let docker = Arc::new(FakeDocker::with_volume("app", "pgdata", b"rows", "postgres:16", "arm64"));
    let x = ext(&docker);
    let kr = &mac.ctx.keyring.clone();
    let pointer = spoko_crypto::name_id(kr, "vol", "pgdata");
    let other = spoko_crypto::seal(kr, b"message", br#"{"type":"volume_request","name_id":"not-ours"}"#);
    x.on_message(&mut mac, &other);
    x.on_message(&mut mac, b"garbage");
    assert!(docker.take_log().is_empty(), "unrelated messages do nothing");
    assert!(mac.ctx.api.get_pointer(mac.project_id, &pointer).unwrap().is_none());
    let request = spoko_crypto::seal(kr, b"message", format!(r#"{{"type":"volume_request","name_id":"{pointer}"}}"#).as_bytes());
    x.on_message(&mut mac, &request);
    assert_eq!(docker.take_log(), vec!["stop app-db", "export app_pgdata", "start app-db"]);
    assert!(mac.ctx.api.get_pointer(mac.project_id, &pointer).unwrap().is_some(), "snapshot is on the server");
}

#[test]
fn local_activity_after_a_device_switch_prompts_for_each_volume() {
    let w = world();
    let docker = Arc::new(FakeDocker::default());
    let x = ext(&docker);
    let (mac, _) = w.create(&w.path("m/app"), "[[docker.volume]]\nname = \"pgdata\"\n\n[[docker.volume]]\nname = \"redis\"\n");
    let (mut linux, notes) = w.clone(mac.project_id, &w.path("l/app"));
    x.on_local_activity(&mut linux);
    let seen: Vec<(String, String)> = notes
        .take()
        .into_iter()
        .filter_map(|n| match n {
            Notification::VolumePrompt { volume, from_device, .. } => Some((volume, from_device)),
            _ => None,
        })
        .collect();
    assert_eq!(seen, vec![("pgdata".to_owned(), "mac".to_owned()), ("redis".to_owned(), "mac".to_owned())]);
    // a project without volumes never prompts
    let (bare, _) = w.create(&w.path("m/bare"), "");
    let (mut linux_bare, notes) = w.clone(bare.project_id, &w.path("l/bare"));
    x.on_local_activity(&mut linux_bare);
    assert!(notes.take().is_empty());
    assert!(docker.take_log().is_empty(), "prompts never touch Docker");
}
