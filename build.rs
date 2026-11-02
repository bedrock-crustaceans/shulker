//! Turns on the embedded vanilla files (`cfg(vanilla)`) when `bake-vanilla` has written
//! `vanilla/vanilla.zst`, and passes the install's label (`vanilla/vanilla.label`) on.

use std::fs;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(vanilla)");
    println!("cargo::rerun-if-changed=vanilla/vanilla.zst");
    println!("cargo::rerun-if-changed=vanilla/vanilla.label");
    if fs::metadata("vanilla/vanilla.zst").is_ok_and(|file| file.is_file()) {
        let label =
            fs::read_to_string("vanilla/vanilla.label").unwrap_or_else(|_| "unknown".into());
        println!("cargo::rustc-env=SHULKER_VANILLA_LABEL={}", label.trim());
        println!("cargo::rustc-cfg=vanilla");
    }
}
