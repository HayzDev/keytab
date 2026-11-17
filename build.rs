fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
        let protocols = pkg_config::get_variable("wayland-protocols", "pkgdatadir")
            .expect("install wayland-protocols-devel");
        let xml = std::path::Path::new(&protocols)
            .join("staging/ext-background-effect/ext-background-effect-v1.xml");
        for (mode, name) in [("client-header", "background-effect.h"), ("private-code", "background-effect.c")] {
            assert!(std::process::Command::new("wayland-scanner")
                .arg(mode).arg(&xml).arg(out.join(name)).status()
                .expect("install wayland-devel").success(), "Wayland protocol generation failed");
        }
        let gtk = pkg_config::probe_library("gtk+-wayland-3.0").unwrap();
        let layer = pkg_config::Config::new().atleast_version("0.9").probe("gtk-layer-shell-0").unwrap();
        let wayland = pkg_config::probe_library("wayland-client").unwrap();
        cc::Build::new().includes(gtk.include_paths).includes(layer.include_paths)
            .includes(wayland.include_paths).include(&out)
            .file("src/platform/material.c").file(out.join("background-effect.c"))
            .compile("keytab-material");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rerun-if-changed=src/platform/material.c");
        println!("cargo:rerun-if-changed={}", xml.display());
    }
    tauri_build::build()
}
