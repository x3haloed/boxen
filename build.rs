fn main() {
    if std::env::var_os("CARGO_CFG_COSMO").is_some() {
        println!("cargo:rustc-link-arg=-Wl,--wrap=ulock_wait");
    }
    #[cfg(feature = "ape")]
    {
        let mut args = cosmo_build::manifest_args().expect("Cosmopolitan build metadata");
        if cfg!(feature = "inference-probe") {
            args.extend(["--features".into(), "inference-probe".into()]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        cosmo_build::apeify_with(&args);
    }
}
