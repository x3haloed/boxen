fn main() {
    if std::env::var_os("CARGO_CFG_COSMO").is_some() {
        println!("cargo:rustc-link-arg=-Wl,--wrap=ulock_wait");
    }
    #[cfg(feature = "ape")]
    cosmo_build::apeify();
}
