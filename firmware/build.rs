// links memory.x for the f401re
use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let linker_script_destination = PathBuf::from(env::var("OUT_DIR").unwrap());

    fs::copy("memory.x", linker_script_destination.join("memory.x")).unwrap();

    // tell cargo to re-run this if memory.x changes
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rustc-link-search={}", linker_script_destination.display());
}