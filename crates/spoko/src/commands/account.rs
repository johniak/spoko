use std::io::Write;
use std::path::PathBuf;

use spoko_client::account;
use spoko_crypto::RecoveryKey;

use super::{Ctx, Res, err};
use crate::cli::DeviceCmd;

fn default_name() -> String {
    ["SPOKO_DEVICE_NAME", "HOSTNAME", "COMPUTERNAME"].iter().find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty())).unwrap_or_else(|| "komputer".into())
}

fn save_local(ctx: &Ctx, id: &account::Identity) -> Res<()> {
    let mut local = ctx.local()?;
    local.server_url = Some(id.server_url.clone());
    local.device_id = Some(id.device_id);
    local.device_name = Some(id.device_name.clone());
    local.save(&ctx.home)?;
    Ok(())
}

pub fn login(ctx: &Ctx, server: &str, key: Option<PathBuf>, recovery: Option<&str>, name: Option<String>, out: &mut dyn Write) -> Res<()> {
    std::fs::create_dir_all(&ctx.home)?;
    let name = name.unwrap_or_else(default_name);
    let key = match key {
        Some(k) => k,
        None => spoko_crypto::default_key_path(&ctx.env.home).ok_or("nie znaleziono klucza SSH (~/.ssh/id_ed25519, id_ecdsa, id_rsa); utwórz go: ssh-keygen -t ed25519, albo podaj --key")?,
    };
    let rk = recovery.map(RecoveryKey::parse).transpose().map_err(|_| "nieprawidłowy klucz odzyskiwania")?;
    let (id, new_rk) = account::login(&ctx.home, server, &key, &name, rk.as_ref(), None)?;
    save_local(ctx, &id)?;
    writeln!(out, "Zalogowano jako {} ({}) kluczem {}", id.device_name, id.device_id, id.fingerprint)?;
    if let Some(k) = new_rk {
        writeln!(out, "Klucz odzyskiwania: {}", k.to_display())?;
        writeln!(out, "Zapisz go w bezpiecznym miejscu — otworzy dane, gdyby klucz SSH przepadł.")?;
    }
    Ok(())
}

pub fn device(ctx: &Ctx, cmd: DeviceCmd, out: &mut dyn Write) -> Res<()> {
    match cmd {
        DeviceCmd::List { json } => {
            let id = ctx.identity()?;
            let devices = id.api().devices()?;
            if json {
                let v: Vec<serde_json::Value> = devices
                    .iter()
                    .map(|d| serde_json::json!({"id": d.id, "name": d.name, "revoked": d.revoked, "last_seen": d.last_seen, "this": d.id == id.device_id}))
                    .collect();
                writeln!(out, "{}", serde_json::to_string_pretty(&v)?)?;
            } else {
                for d in devices {
                    let mark = if d.id == id.device_id { " (ten)" } else { "" };
                    let state = if d.revoked { "odebrany" } else { "aktywny" };
                    writeln!(out, "{}{mark}\t{}\t{state}", d.name, d.id)?;
                }
            }
            Ok(())
        }
        DeviceCmd::Remove { device } => {
            let id = ctx.identity()?;
            let devices = id.api().devices()?;
            let target = devices.iter().filter(|d| !d.revoked).find(|d| d.name == device || d.id.to_string() == device).ok_or_else(|| format!("nie ma aktywnego urządzenia „{device}”"))?;
            if target.id == id.device_id {
                return err("to jest ten komputer");
            }
            account::remove_device(&id, target.id)?;
            writeln!(out, "Wylogowano: {}", target.name)?;
            Ok(())
        }
    }
}
