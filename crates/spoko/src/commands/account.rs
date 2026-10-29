use std::io::{BufRead, Write};
use std::time::Duration;

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

pub fn login(ctx: &Ctx, server: &str, token: Option<&str>, recovery: Option<&str>, name: Option<String>, out: &mut dyn Write) -> Res<()> {
    std::fs::create_dir_all(&ctx.home)?;
    let name = name.unwrap_or_else(default_name);
    let Some(token) = token else { return pair(ctx, server, &name, out) };
    let rk = recovery.map(RecoveryKey::parse).transpose().map_err(|_| "nieprawidłowy klucz odzyskiwania")?;
    let (id, new_rk) = account::login_bootstrap(&ctx.home, server, token, &name, rk.as_ref())?;
    save_local(ctx, &id)?;
    writeln!(out, "Zalogowano jako {} ({})", id.device_name, id.device_id)?;
    if let Some(k) = new_rk {
        writeln!(out, "Klucz odzyskiwania: {}", k.to_display())?;
        writeln!(out, "Zapisz go w bezpiecznym miejscu — tylko nim odzyskasz dane bez innego urządzenia.")?;
    }
    Ok(())
}

fn pair(ctx: &Ctx, server: &str, name: &str, out: &mut dyn Write) -> Res<()> {
    let pending = account::pair_start(server, name)?;
    writeln!(out, "Kod parowania: {}\nOdcisk tego urządzenia: {}", pending.code, pending.fingerprint)?;
    writeln!(out, "Na zalogowanym komputerze uruchom: spoko device approve {}", pending.code)?;
    out.flush()?;
    let id = pending.wait(&ctx.home, Duration::from_secs(600), Duration::from_secs(2))?;
    save_local(ctx, &id)?;
    writeln!(out, "Sparowano: {} ({})", id.device_name, id.device_id)?;
    Ok(())
}

pub fn device(ctx: &Ctx, cmd: DeviceCmd, out: &mut dyn Write) -> Res<()> {
    match cmd {
        DeviceCmd::Add { server, name } => {
            std::fs::create_dir_all(&ctx.home)?;
            pair(ctx, &server, &name.unwrap_or_else(default_name), out)
        }
        DeviceCmd::Approve { code, yes } => {
            let id = ctx.identity()?;
            let ok = account::pair_approve(&id, &code, |name, fp| {
                println!("Urządzenie „{name}”, odcisk {fp}. Czy to ten sam odcisk co na nowym urządzeniu? [t/N]");
                if yes {
                    return true;
                }
                let mut line = String::new();
                let _ = std::io::stdin().lock().read_line(&mut line);
                matches!(line.trim(), "t" | "T" | "tak" | "y" | "Y" | "yes")
            })?;
            writeln!(out, "{}", if ok { "Zatwierdzono." } else { "Odrzucono." })?;
            Ok(())
        }
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
        DeviceCmd::Revoke { device } => {
            let mut id = ctx.identity()?;
            let devices = id.api().devices()?;
            let target = devices.iter().filter(|d| !d.revoked).find(|d| d.name == device || d.id.to_string() == device).ok_or_else(|| format!("nie ma aktywnego urządzenia „{device}”"))?;
            if target.id == id.device_id {
                return err("nie można odebrać dostępu temu urządzeniu");
            }
            let rk = account::revoke_device(&mut id, &ctx.home, target.id)?;
            writeln!(out, "Odebrano dostęp: {}. Klucz danych zrotowany.", target.name)?;
            writeln!(out, "Klucz odzyskiwania: {}", rk.to_display())?;
            Ok(())
        }
    }
}
