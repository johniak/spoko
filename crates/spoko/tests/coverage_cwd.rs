//! Changes the process working directory, so it lives alone in its own test binary.

#[cfg(unix)]
#[test]
fn context_falls_back_to_dot_when_the_working_directory_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    std::env::set_current_dir(&gone).unwrap();
    std::fs::remove_dir(&gone).unwrap();
    let ctx = spoko::commands::Ctx::from_env();
    std::env::set_current_dir(dir.path()).unwrap();
    assert_eq!(ctx.cwd, std::path::PathBuf::from("."));
}
