use std::env;

fn main() {
    println!("cargo:rerun-if-env-changed=MPV_NO_PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=MPV_LIBRARY_DIR");

    // Explicit override: MPV_LIBRARY_DIR points at a directory containing the
    // mpv library (libmpv.so / libmpv.dylib / mpv.lib).
    if let Ok(dir) = env::var("MPV_LIBRARY_DIR") {
        println!("cargo:rustc-link-search=native={dir}");
        println!("cargo:rustc-link-lib=mpv");
        return;
    }

    if env::var_os("MPV_NO_PKG_CONFIG").is_none() && pkg_config::Config::new().probe("mpv").is_ok()
    {
        // pkg-config emitted the link flags.
        return;
    }

    println!("cargo:rustc-link-lib=mpv");
}
