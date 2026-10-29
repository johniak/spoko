use std::io::{Read, Write};
use std::path::Path;

use spoko_core::config::ProjectConfig;
use spoko_core::filter::{Class, Matcher};
use spoko_core::platform::RelPath;

use super::{Ctx, Res, fmt_size};

#[derive(Default)]
struct Tally {
    files: u64,
    bytes: u64,
}

impl Tally {
    fn add(&mut self, size: u64) {
        self.files += 1;
        self.bytes += size;
    }
}

fn head(path: &Path) -> Vec<u8> {
    let mut buf = vec![0u8; 8000];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf.truncate(n);
    buf
}

/// `spoko config check`: what the current rules do to this directory.
pub fn check(ctx: &Ctx, out: &mut dyn Write) -> Res<()> {
    let root = ctx.current_project().map_or_else(|_| ctx.cwd.clone(), |p| p.1);
    let cfg = ProjectConfig::load(&root)?;
    let m = Matcher::new(&root, &cfg)?;
    let (mut synced, mut binary, mut watch, mut ignored) = (Tally::default(), Tally::default(), Tally::default(), Tally::default());
    let mut ignored_dirs: Vec<String> = Vec::new();
    let mut largest: Vec<(u64, String)> = Vec::new();
    let mut stack: Vec<Option<RelPath>> = vec![None];
    while let Some(dir) = stack.pop() {
        let abs = dir.as_ref().map_or_else(|| root.clone(), |d| d.to_fs(&root));
        for entry in std::fs::read_dir(&abs)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue;
            }
            let rel = dir.as_ref().map_or_else(|| RelPath::new(&name), |d| d.join(&name));
            let ft = entry.file_type()?;
            if ft.is_dir() {
                if m.should_descend(&rel) {
                    stack.push(Some(rel));
                } else {
                    ignored_dirs.push(format!("{rel}/"));
                }
                continue;
            }
            let size = entry.metadata()?.len();
            if m.classify(&rel, false) == Class::Ignored {
                ignored.add(size);
                continue;
            }
            synced.add(size);
            if m.is_binary(&rel, &head(&entry.path()), size) {
                binary.add(size);
            }
            if !m.watch_hooks_for(&rel).is_empty() {
                watch.add(size);
            }
            largest.push((size, rel.to_string()));
        }
    }
    ignored_dirs.sort();
    largest.sort_by(|a, b| b.cmp(a));
    writeln!(out, "Katalog: {}", root.display())?;
    writeln!(out, "{:<22}{:>8}{:>12}", "Klasa", "Pliki", "Rozmiar")?;
    for (label, t) in [("Synchronizowane", &synced), ("  w tym Binarne", &binary), ("  w tym Watch", &watch), ("Ignorowane", &ignored)] {
        writeln!(out, "{label:<22}{:>8}{:>12}", t.files, fmt_size(t.bytes))?;
    }
    if !ignored_dirs.is_empty() {
        writeln!(out, "Ignorowane katalogi (bez zaglądania): {}", ignored_dirs.iter().take(10).cloned().collect::<Vec<_>>().join(", "))?;
    }
    writeln!(out, "Największe synchronizowane:")?;
    for (size, p) in largest.iter().take(10) {
        writeln!(out, "  {:>10}  {p}", fmt_size(*size))?;
    }
    Ok(())
}
