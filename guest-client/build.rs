fn main() {
    #[cfg(feature = "ape")]
    cosmo_build::apeify();

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var_os("CARGO_CFG_COSMO").is_none()
    {
        println!("cargo:rerun-if-changed=src/mac_ui.m");
        cc::Build::new()
            .file("src/mac_ui.m")
            .flag("-fobjc-arc")
            .compile("guest_chat_appkit");
        println!("cargo:rustc-link-lib=framework=AppKit");
    }
}
