use std::env;

const VERSION_ENV: &str = "CCUSAGE_VERSION";

fn main() {
    println!("cargo:rerun-if-env-changed={VERSION_ENV}");
    let version = env::var(VERSION_ENV)
        .unwrap_or_else(|_| env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION"));
    println!("cargo:rustc-env={VERSION_ENV}={version}");
}
