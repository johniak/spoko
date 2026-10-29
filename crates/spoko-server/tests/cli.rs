use spoko_server::cli::run_cli;

fn run(args: &[&str]) -> (Result<(), String>, String) {
    let mut out = Vec::new();
    let r = run_cli(args.iter().map(|s| s.to_string()), &mut out).map_err(|e| e.to_string());
    (r, String::from_utf8(out).unwrap())
}

#[test]
fn account_create_and_set_key() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().to_str().unwrap();
    let pub_file = spoko_crypto::test_util::key_path("id_ed25519.pub");
    let (r, out) = run(&["spoko-server", "account", "create", "jan", "--ssh-key", pub_file.to_str().unwrap(), "--data", data]);
    r.unwrap();
    let key = spoko_crypto::test_util::key("id_ed25519");
    assert!(out.contains(&key.fingerprint()), "{out}");
    let db = spoko_server::db::Db::open(&dir.path().join("spoko.db")).unwrap();
    let acc = db.account_by_key(&key.public_openssh()).unwrap().unwrap();
    assert_eq!(db.account_by_name("jan").unwrap(), Some(acc));
    drop(db);

    let (r, _) = run(&["spoko-server", "account", "create", "jan2", "--ssh-key", pub_file.to_str().unwrap(), "--data", data]);
    assert_eq!(r.unwrap_err(), "this SSH key already owns an account");
    let (r, _) = run(&["spoko-server", "account", "create", "x", "--ssh-key", "ssh-ed25519 nope", "--data", data]);
    assert_eq!(r.unwrap_err(), "not an SSH public key: ssh-ed25519 nope");

    let other = spoko_crypto::test_util::key("other_ed25519");
    let line = std::fs::read_to_string(spoko_crypto::test_util::key_path("other_ed25519.pub")).unwrap();
    let (r, out) = run(&["spoko-server", "account", "set-key", "jan", "--ssh-key", line.trim(), "--data", data]);
    r.unwrap();
    assert!(out.contains(&other.fingerprint()), "{out}");
    let db = spoko_server::db::Db::open(&dir.path().join("spoko.db")).unwrap();
    assert_eq!(db.account_by_key(&other.public_openssh()).unwrap(), Some(acc));
    drop(db);
    let (r, _) = run(&["spoko-server", "account", "set-key", "nobody", "--ssh-key", line.trim(), "--data", data]);
    assert_eq!(r.unwrap_err(), "no such account: nobody");
}

#[test]
fn bad_args_error() {
    let (r, _) = run(&["spoko-server", "frobnicate"]);
    assert!(r.is_err());
}

#[test]
fn serve_rejects_bad_listen_address() {
    let dir = tempfile::tempdir().unwrap();
    let (r, _) = run(&["spoko-server", "serve", "--data", dir.path().to_str().unwrap(), "--listen", "not-an-addr"]);
    assert!(r.is_err());
}

#[test]
fn serve_runs_until_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let data = dir.path().to_path_buf();
    let t = std::thread::spawn(move || {
        spoko_server::cli::serve(&data, &format!("127.0.0.1:{port}"), async {
            let _ = rx.await;
        })
        .map_err(|e| e.to_string())
    });
    let url = format!("http://127.0.0.1:{port}/v1/devices");
    let mut status = 0;
    for _ in 0..100 {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        if let Ok(r) = agent.get(&url).call() {
            status = r.status().as_u16();
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(status, 401);
    tx.send(()).unwrap();
    t.join().unwrap().unwrap();
}

#[test]
fn serve_fails_when_port_taken() {
    let dir = tempfile::tempdir().unwrap();
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = busy.local_addr().unwrap().to_string();
    assert!(spoko_server::cli::serve(dir.path(), &addr, async {}).is_err());
}
