fn main() {
    spoko_tray::os_glue::event_loop::run(spoko_core_home());
}

fn spoko_core_home() -> std::path::PathBuf {
    spoko_client::daemon::ipc::default_home()
}
