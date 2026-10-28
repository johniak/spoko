use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Writes a file readable only by the current user (Unix 0600, Windows owner-only ACL).
/// The file is replaced atomically so a crash never leaves a half-written key.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().expect("key file path has a parent");
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".spoko-tmp-{}", data_encoding::HEXLOWER.encode(&crate::random_bytes::<8>())));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    #[cfg(windows)]
    crate::os_glue::restrict_to_owner(&tmp)?;
    fs::rename(&tmp, path)
}

pub fn read_private_file(path: &Path) -> io::Result<Vec<u8>> {
    fs::read(path)
}
