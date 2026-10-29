#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use spoko_client::account;
use spoko_client::engine::{DeviceCtx, Engine, JoinReport};
use spoko_client::notifier::RecordingNotifier;
use spoko_core::external::ExternalEnv;
use spoko_crypto::RecoveryKey;
use spoko_server::test_util::{TestServer, spawn_test_server};
use uuid::Uuid;

/// One test server shared by N devices of one account; each device has its own home and project dir.
pub struct World {
    pub srv: TestServer,
    /// The account's SSH key (test key name) and its passphrase.
    key: (&'static str, Option<&'static str>),
    rk: Option<RecoveryKey>,
    pub tmp: tempfile::TempDir,
    /// Shared monotonic clock: every call is one second later, across all devices.
    clock: Arc<AtomicI64>,
}

pub struct Device {
    pub name: String,
    pub home: PathBuf,
    pub root: PathBuf,
    pub claude: PathBuf,
    pub ctx: DeviceCtx,
    pub notes: Arc<RecordingNotifier>,
}

impl World {
    pub fn new() -> World {
        World::with_key("id_ed25519", None)
    }

    pub fn with_key(key: &'static str, passphrase: Option<&'static str>) -> World {
        World {
            srv: spawn_test_server(),
            key: (key, passphrase),
            rk: None,
            tmp: tempfile::tempdir().unwrap(),
            clock: Arc::new(AtomicI64::new(1_700_000_000)),
        }
    }

    pub fn device(&mut self, name: &str) -> Device {
        self.device_at(name, &self.tmp.path().join(name).join("proj"))
    }

    pub fn device_at(&mut self, name: &str, root: &Path) -> Device {
        let base = self.tmp.path().join(name);
        let home = base.join("spoko-home");
        std::fs::create_dir_all(&home).unwrap();
        // every machine of the account has the same SSH key
        let (key, pass) = self.key;
        let path = spoko_crypto::test_util::key_path(key);
        self.srv.create_account("jan", &spoko_crypto::SshKey::load(&path, pass).unwrap());
        let (id, rk) = account::login(&home, &self.srv.url, &path, name, None, pass).unwrap();
        if self.rk.is_none() {
            self.rk = rk;
        }
        let claude = base.join("claude");
        let env = ExternalEnv {
            home: base.join("home"),
            claude_config_dir: Some(claude.clone()),
        };
        let notes = Arc::new(RecordingNotifier::default());
        let mut ctx = DeviceCtx::new(&home, &id, env, notes.clone());
        let clock = self.clock.clone();
        ctx.clock = Arc::new(move || clock.fetch_add(1, Ordering::SeqCst));
        Device {
            name: name.into(),
            home,
            root: root.to_path_buf(),
            claude,
            ctx,
            notes,
        }
    }
}

impl Device {
    pub fn create(&self) -> Engine {
        std::fs::create_dir_all(&self.root).unwrap();
        Engine::create(self.ctx.clone(), &self.root, "proj").unwrap()
    }
    pub fn join(&self, project: Uuid) -> (Engine, JoinReport) {
        Engine::clone_into(self.ctx.clone(), project, &self.root).unwrap()
    }
    pub fn open(&self, project: Uuid) -> Engine {
        Engine::open(self.ctx.clone(), project, &self.root).unwrap()
    }
    pub fn write(&self, rel: &str, content: impl AsRef<[u8]>) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }
    pub fn read(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(rel)).ok()
    }
    pub fn text(&self, rel: &str) -> String {
        String::from_utf8(
            self.read(rel)
                .unwrap_or_else(|| panic!("{} missing on {}", rel, self.name)),
        )
        .unwrap()
    }
    pub fn rm(&self, rel: &str) {
        std::fs::remove_file(self.root.join(rel)).unwrap();
    }
}

/// Every regular file under `root` except `.git`, as (relative path, bytes).
pub fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                if e.file_name() != ".git" {
                    stack.push(p);
                }
            } else {
                out.push((
                    p.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read(&p).unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

pub fn sig() -> git2::Signature<'static> {
    git2::Signature::new("Jan", "jan@example.com", &git2::Time::new(1_700_000_000, 0)).unwrap()
}

pub fn init_repo(root: &Path) -> git2::Repository {
    let repo = git2::Repository::init(root).unwrap();
    repo.config().unwrap().set_str("user.name", "Jan").unwrap();
    repo.config()
        .unwrap()
        .set_str("user.email", "jan@example.com")
        .unwrap();
    repo
}

pub fn commit_all(repo: &git2::Repository, msg: &str) -> git2::Oid {
    let mut idx = repo.index().unwrap();
    idx.add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    idx.write().unwrap();
    let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
    let parents: Vec<git2::Commit> = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .map(|o| repo.find_commit(o).unwrap())
        .into_iter()
        .collect();
    let refs: Vec<&git2::Commit> = parents.iter().collect();
    repo.commit(Some("HEAD"), &sig(), &sig(), msg, &tree, &refs)
        .unwrap()
}
