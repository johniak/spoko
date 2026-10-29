//! Against a real Docker daemon; run with `SPOKO_DOCKER_TESTS=1` (CI job `docker`).

use std::process::Command;
use std::time::{Duration, Instant};

use spoko_client::account;
use spoko_core::config::{Mismatch as OnMismatch, VolumeCfg, VolumeMode};
use spoko_docker::bollard_api::BollardApi;
use spoko_docker::volume::{self, PullOutcome, VolumeCtx};
use spoko_server::test_util::spawn_test_server;

fn enabled() -> bool {
    let on = std::env::var("SPOKO_DOCKER_TESTS").is_ok_and(|v| v == "1");
    if !on {
        eprintln!("skipped: set SPOKO_DOCKER_TESTS=1");
    }
    on
}

fn docker(args: &[&str]) -> String {
    let o = Command::new("docker").args(args).output().expect("docker CLI");
    assert!(o.status.success(), "docker {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}

fn psql(container: &str, sql: &str) -> String {
    docker(&["exec", container, "psql", "-U", "postgres", "-tAc", sql])
}

fn wait_ready(container: &str) {
    let deadline = Instant::now() + Duration::from_secs(90);
    while Instant::now() < deadline {
        let ok = Command::new("docker").args(["exec", container, "psql", "-U", "postgres", "-tAc", "select 1"]).output().is_ok_and(|o| o.status.success());
        if ok {
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    panic!("postgres in {container} not ready");
}

struct Cleanup(Vec<String>, Vec<String>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for c in &self.0 {
            let _ = Command::new("docker").args(["rm", "-f", c]).output();
        }
        for v in &self.1 {
            let _ = Command::new("docker").args(["volume", "rm", "-f", v]).output();
        }
    }
}

fn pg_container(name: &str, volume: &str, start: bool) {
    let mount = format!("{volume}:/var/lib/postgresql/data");
    let mut args = vec![if start { "run" } else { "create" }];
    if start {
        args.push("-d");
    }
    args.extend(["--name", name, "-e", "POSTGRES_PASSWORD=x", "-v", &mount, "postgres:16"]);
    docker(&args);
}

fn roundtrip(mode: VolumeMode) {
    let tag = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
    let (vol, c1, c2) = (format!("spoko_it_{tag}"), format!("spoko-it-a-{tag}"), format!("spoko-it-b-{tag}"));
    let _cleanup = Cleanup(vec![c1.clone(), c2.clone()], vec![vol.clone()]);
    let srv = spawn_test_server();
    let home = tempfile::tempdir().unwrap();
    let (id, _) = account::login_bootstrap(home.path(), &srv.url, &srv.bootstrap_token("jan"), "ci", None).unwrap();
    let api = id.api();
    let project = api.create_project(b"m").unwrap().id;
    let d = BollardApi::connect().unwrap();
    let ctx = VolumeCtx { docker: &d, api: &api, keyring: &id.keyring, project, project_hint: "it", device_id: id.device_id, device_name: "ci", now: 1 };
    let cfg = VolumeCfg {
        name: vol.clone(),
        mode,
        stop_container: true,
        dump: Some("pg_dump -U postgres -Fc postgres".into()),
        restore: Some("pg_restore -U postgres -d postgres --clean --if-exists".into()),
        on_mismatch: OnMismatch::Warn,
    };

    pg_container(&c1, &vol, true);
    wait_ready(&c1);
    psql(&c1, "create table t(v text); insert into t values ('hello from spoko')");
    let rep = volume::push(&ctx, &cfg).unwrap();
    if mode == VolumeMode::Raw {
        assert_eq!(rep.meta.pg_major.as_deref(), Some("16"));
        assert_eq!(docker(&["inspect", "-f", "{{.State.Running}}", &c1]), "true", "restarted after export");
    }
    docker(&["rm", "-f", &c1]);
    docker(&["volume", "rm", &vol]);

    pg_container(&c2, &vol, mode == VolumeMode::Dump);
    if mode == VolumeMode::Dump {
        wait_ready(&c2);
    }
    assert!(matches!(volume::pull(&ctx, &cfg, false).unwrap(), PullOutcome::Restored(_)));
    if mode == VolumeMode::Raw {
        docker(&["start", &c2]);
    }
    wait_ready(&c2);
    assert_eq!(psql(&c2, "select v from t"), "hello from spoko");
}

#[test]
fn postgres_raw_roundtrip() {
    if enabled() {
        roundtrip(VolumeMode::Raw);
    }
}

#[test]
fn postgres_dump_roundtrip() {
    if enabled() {
        roundtrip(VolumeMode::Dump);
    }
}
