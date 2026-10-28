use std::io;
use std::path::Path;
use std::process::Command;

/// Removes inherited ACEs and grants full control to the current user only.
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    let user = std::env::var("USERNAME").map_err(io::Error::other)?;
    let status = Command::new("icacls")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &format!("{user}:F")])
        .stdout(std::process::Stdio::null())
        .status()?;
    if status.success() { Ok(()) } else { Err(io::Error::other("icacls failed")) }
}
