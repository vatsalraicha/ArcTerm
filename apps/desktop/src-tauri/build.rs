//! Build script. Runs the standard Tauri codegen and, on Windows, stages
//! the llama.cpp / ggml shared libraries where the bundler can find them.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    stage_ggml_runtime();
    tauri_build::build();
}

/// Windows: copy `llama.dll`, `ggml*.dll` and the per-ISA / Vulkan backend
/// modules into `gen/ggml-runtime/` so `tauri.windows.conf.json` can glob
/// them into the installer next to the executable (`arcterm-desktop.exe`).
///
/// Why a staging directory instead of pointing the bundler at the build
/// output: llama-cpp-sys-2's OUT_DIR carries a content hash in its name,
/// so there is no stable path to put in a config file, and a cached
/// `target/` can hold stale sibling OUT_DIRs from earlier feature sets.
/// `DEP_LLAMA_BACKENDS_DIR` is the one authoritative location: the sys
/// crate's build script publishes it to immediate dependents (hence the
/// direct `llama-cpp-sys-2` dependency in Cargo.toml). Must run before
/// `tauri_build::build()`, which resolves the resource globs and fails on
/// a glob with no matches.
///
/// Copies are content-compared and only rewritten when they differ.
/// tauri-build registers every resource as `rerun-if-changed`, so
/// blindly rewriting the files on each run would re-trigger this script
/// (and a full recompile of the crate) on every single cargo invocation.
fn stage_ggml_runtime() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-env-changed=DEP_LLAMA_BACKENDS_DIR");
    let backends_dir = PathBuf::from(std::env::var_os("DEP_LLAMA_BACKENDS_DIR").expect(
        "DEP_LLAMA_BACKENDS_DIR is unset: the Windows build needs llama-cpp-2's \
         `dynamic-backends` feature (see the Windows dependency table in Cargo.toml)",
    ));
    // CMake installs the core libraries (llama, ggml, ggml-base) to
    // <out>/bin and the loadable backend modules to <out>/backends.
    let bin_dir = backends_dir
        .parent()
        .expect("backends dir has a parent")
        .join("bin");
    let dest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("gen")
        .join("ggml-runtime");
    fs::create_dir_all(&dest).expect("create gen/ggml-runtime");

    let mut wanted = Vec::new();
    for dir in [&bin_dir, &backends_dir] {
        println!("cargo:rerun-if-changed={}", dir.display());
        let entries = fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
        for entry in entries.flatten() {
            let src = entry.path();
            if src.extension().and_then(|e| e.to_str()) != Some("dll") {
                continue;
            }
            let name = src.file_name().unwrap().to_owned();
            let target = dest.join(&name);
            if !same_contents(&src, &target) {
                fs::copy(&src, &target)
                    .unwrap_or_else(|e| panic!("copy {} -> {}: {e}", src.display(), target.display()));
            }
            wanted.push(name);
        }
    }
    assert!(
        wanted.iter().any(|n| n.to_string_lossy().starts_with("ggml-cpu-")),
        "no ggml-cpu-*.dll in {}: GGML_CPU_ALL_VARIANTS did not build",
        backends_dir.display()
    );

    // Drop leftovers from an earlier build (renamed variants, a backend
    // that was switched off) so the installer never ships a stale DLL.
    for entry in fs::read_dir(&dest).expect("read gen/ggml-runtime").flatten() {
        if !wanted.contains(&entry.file_name()) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn same_contents(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(ma), Ok(mb)) if ma.len() == mb.len() => {
            matches!((fs::read(a), fs::read(b)), (Ok(x), Ok(y)) if x == y)
        }
        _ => false,
    }
}
