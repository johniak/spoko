use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};

use crate::{AppState, app, blobstore::BlobStore, db::Db, hub::Hub};

pub type CliResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(name = "spoko-server", version, about = "spoko sync server")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the HTTP/WebSocket server.
    Serve {
        #[arg(long, default_value = "./spoko-data")]
        data: PathBuf,
        #[arg(long, default_value = "0.0.0.0:7420")]
        listen: String,
        /// The URL clients log in with (e.g. https://spoko.example.com). Login signatures are
        /// bound to it; without it the request's Host header is trusted.
        #[arg(long)]
        public_url: Option<String>,
    },
    /// Manage accounts.
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Create an account owned by an SSH key (every machine with that key can log in).
    Create {
        name: String,
        /// Public key: `ssh-ed25519 AAAA…` or a path to the `.pub` file.
        #[arg(long)]
        ssh_key: String,
        #[arg(long, default_value = "./spoko-data")]
        data: PathBuf,
    },
    /// Replace the account's SSH key (lost or leaked key); logs every device out.
    SetKey {
        name: String,
        #[arg(long)]
        ssh_key: String,
        #[arg(long, default_value = "./spoko-data")]
        data: PathBuf,
    },
}

fn open_db(data: &Path) -> Result<Db, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(data)?;
    Ok(Db::open(&data.join("spoko.db"))?)
}

pub fn run_cli(args: impl IntoIterator<Item = String>, out: &mut dyn Write) -> CliResult {
    match Cli::try_parse_from(args)?.cmd {
        Cmd::Serve { data, listen, public_url } => serve(&data, &listen, public_url, async {
            let _ = tokio::signal::ctrl_c().await;
        }),
        Cmd::Account { cmd: AccountCmd::Create { name, ssh_key, data } } => {
            let (key, fp) = public_key(&ssh_key)?;
            let db = open_db(&data)?;
            if db.account_by_key(&key)?.is_some() {
                return Err("this SSH key already owns an account".into());
            }
            db.create_account(&name, &key, &fp)?;
            writeln!(out, "account {name} created for {fp}\nlog in on each machine with: spoko login <server url>")?;
            Ok(())
        }
        Cmd::Account { cmd: AccountCmd::SetKey { name, ssh_key, data } } => {
            let (key, fp) = public_key(&ssh_key)?;
            let db = open_db(&data)?;
            let acc = db.account_by_name(&name)?.ok_or_else(|| format!("no such account: {name}"))?;
            db.set_account_key(acc, &key, &fp)?;
            writeln!(out, "account {name} now uses {fp}; devices were logged out\nlog in with the new key and: spoko login --recovery-key …")?;
            Ok(())
        }
    }
}

/// A key line, or a path to a `.pub` file.
fn public_key(arg: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let line = if Path::new(arg).is_file() { std::fs::read_to_string(arg)? } else { arg.to_owned() };
    spoko_crypto::parse_public_key(&line).map_err(|e| match e {
        spoko_crypto::CryptoError::UnsupportedKey => {
            let alg = line.split_whitespace().next().unwrap_or_default();
            format!("spoko cannot log in with {alg} keys; use an Ed25519, ECDSA or RSA key kept in a file (not DSA, not a hardware sk-* key)").into()
        }
        _ => format!("not an SSH public key: {}", line.trim()).into(),
    })
}

/// Runs the server until `shutdown` resolves.
pub fn serve(data: &Path, listen: &str, public_url: Option<String>, shutdown: impl Future<Output = ()> + Send + 'static) -> CliResult {
    let addr: SocketAddr = listen.parse()?;
    let state = Arc::new(AppState { db: open_db(data)?, blobs: BlobStore::open(&data.join("blobs"))?, hub: Hub::default(), public_url });
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!("spoko-server listening on {addr}");
        axum::serve(listener, app(state)).with_graceful_shutdown(shutdown).await
    })?;
    Ok(())
}
