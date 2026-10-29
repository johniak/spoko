use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const TMP_PREFIX: &str = ".spoko-tmp-";

pub fn is_tmp_name(name: &str) -> bool {
    name.starts_with(TMP_PREFIX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskKind {
    File,
    Symlink(String),
    Dir,
}

/// What `lstat` says about a path; symlinks are never followed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskEntry {
    pub kind: DiskKind,
    pub size: u64,
    pub mtime_ns: i64,
    pub exec: bool,
    pub ino: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymlinkOutcome {
    Link,
    /// Symlinks unavailable (Windows without privilege): a regular file holding the target.
    Placeholder,
}

fn mtime_ns(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
}

#[cfg(unix)]
fn ino_and_exec(meta: &fs::Metadata) -> (u64, bool) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    (meta.ino(), meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn ino_and_exec(_meta: &fs::Metadata) -> (u64, bool) {
    (0, false)
}

pub fn read_entry(path: &Path) -> io::Result<Option<DiskEntry>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => return Ok(None),
        Err(e) => return Err(e),
    };
    let ft = meta.file_type();
    let kind = if ft.is_symlink() {
        DiskKind::Symlink(fs::read_link(path)?.to_string_lossy().replace('\\', "/"))
    } else if ft.is_dir() {
        DiskKind::Dir
    } else {
        DiskKind::File
    };
    let (ino, exec) = ino_and_exec(&meta);
    Ok(Some(DiskEntry { kind, size: meta.len(), mtime_ns: mtime_ns(&meta), exec, ino }))
}

fn tmp_path(path: &Path) -> PathBuf {
    path.with_file_name(format!("{TMP_PREFIX}{}", uuid::Uuid::new_v4().simple()))
}

/// Retries transient Windows sharing violations (file open in an editor / indexer).
fn with_retry(mut op: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(e) if cfg!(windows) && e.kind() == io::ErrorKind::PermissionDenied && attempt < 5 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// Puts the file's data on the drive. On Apple `sync_all` is `F_FULLFSYNC` (~5 ms: it flushes
/// the whole drive cache), which would make a 100k-file checkout take minutes; plain `fsync`
/// orders the data before the rename, and the `F_FULLFSYNC` of the state file saved after an
/// apply is the barrier that flushes the drive cache.
fn durable(f: &fs::File) -> io::Result<()> {
    #[cfg(target_vendor = "apple")]
    return rustix::fs::fsync(f).map_err(io::Error::from);
    #[cfg(not(target_vendor = "apple"))]
    f.sync_all()
}

/// Writes via a temp file in the same directory, fsync, then rename over `path`.
pub fn atomic_write(path: &Path, reader: &mut dyn Read, exec: bool) -> io::Result<DiskEntry> {
    atomic_write_with(path, exec, &mut |f| io::copy(reader, f).map(|_| ()))
}

/// [`atomic_write`] that also flushes the drive cache (`F_FULLFSYNC` on Apple): a barrier making
/// every earlier write durable too. For state files that record what was applied.
pub fn atomic_write_barrier(path: &Path, bytes: &[u8]) -> io::Result<()> {
    atomic_write(path, &mut &bytes[..], false)?;
    fs::File::open(path)?.sync_all()
}

/// [`atomic_write`] where the content is produced by `fill`.
pub fn atomic_write_with(path: &Path, exec: bool, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<DiskEntry> {
    fs::create_dir_all(path.parent().expect("file has a parent"))?;
    atomic_write_in_dir(path, exec, fill)
}

/// [`atomic_write_with`] for a path whose parent directory already exists.
pub fn atomic_write_in_dir(path: &Path, exec: bool, fill: &mut dyn FnMut(&mut dyn Write) -> io::Result<()>) -> io::Result<DiskEntry> {
    let tmp = tmp_path(path);
    let result = (|| {
        let mut f = io::BufWriter::new(fs::File::create(&tmp)?);
        fill(&mut f)?;
        let f = f.into_inner().map_err(io::IntoInnerError::into_error)?;
        set_exec(&f, exec)?;
        durable(&f)?;
        drop(f);
        with_retry(|| fs::rename(&tmp, path))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result?;
    Ok(read_entry(path)?.expect("just written"))
}

/// A fresh temp file is never executable (0666 & ~umask), so only `exec` needs a chmod.
#[cfg(unix)]
fn set_exec(f: &fs::File, exec: bool) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if exec { f.set_permissions(fs::Permissions::from_mode(0o755)) } else { Ok(()) }
}

#[cfg(not(unix))]
fn set_exec(_f: &fs::File, _exec: bool) -> io::Result<()> {
    Ok(())
}

pub fn write_symlink(path: &Path, target: &str) -> io::Result<SymlinkOutcome> {
    let dir = path.parent().expect("link has a parent");
    fs::create_dir_all(dir)?;
    let tmp = tmp_path(path);
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, &tmp)?;
        fs::rename(&tmp, path)?;
        Ok(SymlinkOutcome::Link)
    }
    #[cfg(windows)]
    {
        let native = target.replace('/', "\\");
        if std::os::windows::fs::symlink_file(&native, &tmp).is_ok() {
            with_retry(|| fs::rename(&tmp, path))?;
            return Ok(SymlinkOutcome::Link);
        }
        atomic_write(path, &mut target.as_bytes(), false)?;
        Ok(SymlinkOutcome::Placeholder)
    }
}

/// Removes a file; a missing file is not an error.
pub fn remove_file(path: &Path) -> io::Result<()> {
    with_retry(|| match fs::remove_file(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    })
}

/// Deletes orphaned temp files left by a crash; `skip_dir` prunes ignored trees.
pub fn cleanup_tmp(root: &Path, skip_dir: &dyn Fn(&Path) -> bool) -> io::Result<usize> {
    let mut removed = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                if !skip_dir(&path) && entry.file_name() != ".git" {
                    stack.push(path);
                }
            } else if is_tmp_name(&entry.file_name().to_string_lossy()) {
                fs::remove_file(&path)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}
