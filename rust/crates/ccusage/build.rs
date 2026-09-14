use std::{env, fs};

use serde_json::Value;

const PACKAGE_JSON: &str = "../../../package.json";
const VERSION_ENV: &str = "CCUSAGE_VERSION";

fn main() {
    println!("cargo:rerun-if-env-changed={VERSION_ENV}");
    println!("cargo:rerun-if-changed={PACKAGE_JSON}");
    let version = env::var(VERSION_ENV).unwrap_or_else(|_| {
        // The root package.json is absent when building from a crates.io tarball;
        // fall back to this crate's own version in that case.
        fs::read_to_string(PACKAGE_JSON)
            .ok()
            .and_then(|package_json| {
                serde_json::from_str::<Value>(&package_json)
                    .ok()
                    .and_then(|package| package.get("version")?.as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION"))
    });
    println!("cargo:rustc-env={VERSION_ENV}={version}");
}
