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
    let pass = if spoko_crypto::SshKey::is_encrypted_file(&key)? { Some(ctx.ask_passphrase(&key)?) } else { None };
    let (id, new_rk) = account::login(&ctx.home, server, &key, &name, rk.as_ref(), pass.as_deref())?;
    save_local(ctx, &id)?;
    super::daemon::restart_if_running(ctx, out)?;
    writeln!(out, "Zalogowano jako {} ({}) kluczem {}", id.device_name, id.device_id, id.fingerprint)?;
    if let Some(k) = new_rk {
        writeln!(out, "Klucz odzyskiwania: {}", k.to_display())?;
        writeln!(out, "Zapisz go w bezpiecznym miejscu — otworzy dane, gdyby klucz SSH przepadł.")?;
    }
    Ok(())
}

pub fn rotate_key(ctx: &Ctx, new: &std::path::Path, out: &mut dyn Write) -> Res<()> {
    let mut id = ctx.identity()?;
    let pass = if spoko_crypto::SshKey::is_encrypted_file(new)? { Some(ctx.ask_passphrase(new)?) } else { None };
    let rk = account::rotate_key(&mut id, &ctx.home, new, pass.as_deref())?;
    writeln!(out, "Konto używa teraz klucza {}; stary klucz już nie działa.", id.fingerprint)?;
    writeln!(out, "Nowy klucz odzyskiwania: {}", rk.to_display())?;
    writeln!(out, "Zapisz go — poprzedni otwiera już tylko dane sprzed zmiany.")?;
    writeln!(out, "Pozostałe komputery: skopiuj nowy klucz i uruchom spoko login {}", id.server_url)?;
    super::daemon::restart_if_running(ctx, out)
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
