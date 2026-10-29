//! In-process server for tests of the server and of the client.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::{AppState, Shared, app};

pub struct TestServer {
    pub url: String,
    pub addr: SocketAddr,
    pub state: Shared,
    _dir: tempfile::TempDir,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    /// Creates an account owned by `key` (an existing account of that key is returned).
    pub fn create_account(&self, name: &str, key: &spoko_crypto::SshKey) -> uuid::Uuid {
        let db = &self.state.db;
        let public = key.public_openssh();
        db.account_by_key(&public).unwrap().unwrap_or_else(|| db.create_account(name, &public, &key.fingerprint()).unwrap())
    }

    /// A logged-in device of the account of test key `key` (see `spoko_crypto::test_util`),
    /// made directly in the database.
    pub fn login(&self, key: &str, device: &str) -> crate::db::DeviceLogin {
        let acc = self.create_account(key, &spoko_crypto::test_util::key(key));
        self.state.db.login_device(acc, device, None).unwrap()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A stopped [`TestServer`] that keeps its data and address.
pub struct StoppedServer {
    addr: SocketAddr,
    dir: tempfile::TempDir,
}

impl TestServer {
    /// Stops the server (open connections are dropped) keeping its data for [`StoppedServer::restart`].
    pub fn stop(mut self) -> StoppedServer {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let dir = std::mem::replace(&mut self._dir, tempfile::tempdir().unwrap());
        StoppedServer { addr: self.addr, dir }
    }
}

impl StoppedServer {
    pub fn restart(self) -> TestServer {
        spawn_in(self.dir, Some(self.addr))
    }
}

pub fn spawn_test_server() -> TestServer {
    spawn_on(None)
}

/// Like [`spawn_test_server`] but optionally on a fixed address (to restart a server in place).
pub fn spawn_on(addr: Option<SocketAddr>) -> TestServer {
    spawn_in(tempfile::tempdir().unwrap(), addr)
}

fn spawn_in(dir: tempfile::TempDir, addr: Option<SocketAddr>) -> TestServer {
    let state: Shared = Arc::new(AppState {
        db: crate::db::Db::open(&dir.path().join("spoko.db")).unwrap(),
        blobs: crate::blobstore::BlobStore::open(&dir.path().join("blobs")).unwrap(),
        hub: crate::hub::Hub::default(),
    });
    let bind = addr.unwrap_or_else(|| "127.0.0.1:0".parse().unwrap());
    // the old listener may linger briefly after a stop
    let mut tries = 0;
    let std_listener = loop {
        match std::net::TcpListener::bind(bind) {
            Ok(l) => break l,
            Err(e) if tries < 50 => {
                tries += 1;
                let _ = e;
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => panic!("bind {bind}: {e}"),
        }
    };
    std_listener.set_nonblocking(true).unwrap();
    let addr = std_listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let st = state.clone();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
            axum::serve(listener, app(st)).with_graceful_shutdown(async { let _ = rx.await; }).await.unwrap();
        });
    });
    TestServer { url: format!("http://{addr}"), addr, state, _dir: dir, shutdown: Some(tx), thread: Some(thread) }
}
