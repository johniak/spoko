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
    },
    /// Manage accounts.
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Create an account and print a bootstrap token for its first device.
    Create {
        name: String,
        #[arg(long, default_value = "./spoko-data")]
        data: PathBuf,
    },
    /// Print a new bootstrap token (add a device without pairing, or recover).
    Token {
        name: String,
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
        Cmd::Serve { data, listen } => serve(&data, &listen, async {
            let _ = tokio::signal::ctrl_c().await;
        }),
        Cmd::Account { cmd: AccountCmd::Create { name, data } } => {
            let db = open_db(&data)?;
            let acc = db.create_account(&name)?;
            writeln!(out, "account {name} created\nbootstrap token: {}", db.issue_bootstrap_token(acc)?)?;
            Ok(())
        }
        Cmd::Account { cmd: AccountCmd::Token { name, data } } => {
            let db = open_db(&data)?;
            let acc = db.account_by_name(&name)?.ok_or_else(|| format!("no such account: {name}"))?;
            writeln!(out, "bootstrap token: {}", db.issue_bootstrap_token(acc)?)?;
            Ok(())
        }
    }
}

/// Runs the server until `shutdown` resolves.
pub fn serve(data: &Path, listen: &str, shutdown: impl Future<Output = ()> + Send + 'static) -> CliResult {
    let addr: SocketAddr = listen.parse()?;
    let state = Arc::new(AppState { db: open_db(data)?, blobs: BlobStore::new(data.join("blobs")), hub: Hub::default() });
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        eprintln!("spoko-server listening on {addr}");
        axum::serve(listener, app(state)).with_graceful_shutdown(shutdown).await
    })?;
    Ok(())
}
