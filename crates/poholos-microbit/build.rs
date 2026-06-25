// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Ivan Petrouchtchak

//! Puts `memory.x` where the `cortex-m-rt` linker script can find it.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    fs::write(out.join("memory.x"), include_bytes!("memory.x")).expect("writing memory.x");
    println!("cargo:rustc-link-search={}", out.display());
    println!("cargo:rerun-if-changed=memory.x");
    // The buddy node id is baked in at compile time (see main.rs).
    println!("cargo:rerun-if-env-changed=POHOLOS_BUDDY");
}
