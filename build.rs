use std::{env, fs, path::PathBuf};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    fs::write(out.join("memory.x"), include_bytes!("memory.x")).unwrap();
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rustc-link-arg-bins=--nmagic");
    println!("cargo:rustc-link-arg-bins=-Tlink.x");

    // The image version, in minutes since 1970: with A/B partitions, the boot ROM boots the
    // higher version, so each build must outrank the last (see src/ota.rs).
    let minutes = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() / 60;
    println!("cargo:rustc-env=BADGE_VERSION_MAJOR={}", minutes >> 16);
    println!("cargo:rustc-env=BADGE_VERSION_MINOR={}", minutes & 0xFFFF);
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=ui/src");
}
