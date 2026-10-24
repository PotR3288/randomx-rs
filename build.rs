// Copyright 2019. The Tari Project
//
// Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
// following conditions are met:
//
// 1. Redistributions of source code must retain the above copyright notice, this list of conditions and the following
// disclaimer.
//
// 2. Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the
// following disclaimer in the documentation and/or other materials provided with the distribution.
//
// 3. Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
// products derived from this software without specific prior written permission.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
// INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
// DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
// SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
// SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
// WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE
// USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use std::env;
use std::fs;
use std::path::PathBuf;

use cmake::Config;

#[allow(clippy::too_many_lines)]
fn main() {
    println!("cargo:rerun-if-env-changed=RANDOMX_MSVC_CXX");
    let randomx_dir = env::var("RANDOMX_DIR").unwrap_or_else(|_| "RandomX".to_string());

    // Determine whether we're building with optimizations (release or opt-level > 0).
    let is_optimized = match env::var("OPT_LEVEL") {
        Ok(level) => level != "0",
        Err(_) => false,
    };

    // Build the RandomX C++ library. We pass -flto=thin via environment variables
    // because cmake's CMAKE_INTERPROCEDURAL_OPTIMIZATION is unreliable across toolchains
    // and often silently falls back to non-LTO compilation.
    let randomx_path = if is_optimized {
        // Wipe the cached CMake build tree when the clang-cl override is active.
        // A stale CMakeCache.txt carries CMAKE_CXX_COMPILER as UNINITIALIZED and
        // CMake then reuses its cached compiler instead of re-probing, silently
        // falling back to MSVC despite the -D defines. Nuke out/build so every
        // override run configures from scratch.
        if env::var("RANDOMX_MSVC_CXX").is_ok() {
            let out = env::var("OUT_DIR").expect("OUT_DIR");
            let build_dir = std::path::Path::new(&out).join("build");
            if build_dir.exists() {
                println!("cargo:warning=randomx-rs: wiping cached CMake build tree (RANDOMX_MSVC_CXX active)");
                std::fs::remove_dir_all(&build_dir).expect("failed to wipe cached CMake build dir");
            }
        }
        let mut config = Config::new(&randomx_dir);
        config.define("DARCH", "native");

        // MSVC defaults the C++ support layer to baseline x86-64 (no AVX2/FMA),
        // which costs ~2x per-hash vs GCC's -maes/AVX2-class codegen on the same
        // hardware (measured 409 vs 792 H/s per VM on a Ryzen 3600). Lift the
        // MSVC codegen floor to AVX2. Gated on the *target* env, so mingw/GCC
        // and non-Windows builds are untouched. NOTE: the resulting binary
        // requires an AVX2-capable CPU (Haswell 2013+ / Zen+); a runtime-dispatch
        // variant is the follow-up if that floor is unacceptable.
        // clang-cl discriminator: RANDOMX_MSVC_CXX=<path to clang-cl.exe> swaps the
        // MSVC-side C/C++ compiler for LLVM's MSVC-compatible frontend while keeping
        // the same ABI, flags, and linker. Unset = plain cl (cargo default).
        if let Ok(cxx) = env::var("RANDOMX_MSVC_CXX") {
            let cxxp = std::path::PathBuf::from(&cxx);
            let cc = cxxp.with_file_name("clang-cl.exe");
            let cc = if cc.exists() { cc } else { cxxp.clone() };
            config.define("CMAKE_C_COMPILER", cc.to_str().unwrap());
            config.define("CMAKE_CXX_COMPILER", cxx.as_str());
            // The Visual Studio generator IGNORES -D CMAKE_C/CXX_COMPILER on the
            // command line (compiler is selected by toolset there, so both end up
            // cached as UNINITIALIZED and CMake probes cl.exe anyway). Switch to
            // Ninja, where the -D compiler defines are honored. cmake-rs then
            // skips -Thost=x64/-Ax64 automatically (they are VS-generator-only).
            config.generator("Ninja");
        }
        if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            config.cflag("/arch:AVX2");
            config.cxxflag("/arch:AVX2");
        }

        // Use -flto=thin via compiler flags — works with GCC, Clang, and AppleClang.
        // ThinLTO gives ~80-90% of full LTO speedup at a fraction of compile time.
        // SKIP under clang-cl: -flto=thin emits LLVM bitcode objects that MSVC's
        // link.exe cannot consume (LNK2019 unresolved randomx_* everywhere).
        // clang-cl only supports thin LTO via lld-link; not worth wiring for now.
        let using_clang_cl = env::var("RANDOMX_MSVC_CXX").is_ok()
            && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
        let is_msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
        // LTO env flags only on GCC/Clang (unix) targets: cl.exe D9002-ignores them and
        // CMAKE_INTERPROCEDURAL_OPTIMIZATION under MSVC produced LNK2019 on
        // randomx_release_dataset / randomx_get_flags. MSVC gets /arch:AVX2 only.
        if !using_clang_cl && !is_msvc {
            let lto_flags = "-flto=thin";
            config.env("CFLAGS", lto_flags).env("CXXFLAGS", lto_flags);
        }

        // Also try cmake IPO as a secondary mechanism (works on some toolchains).
        // Same clang-cl caveat as the -flto=thin env flags above: IPO with clang-cl
        // produces LLVM bitcode objects that MSVC link.exe cannot consume (LNK2019).
        // Skip IPO for clang-cl; regular cl.exe and GCC keep it.
        if !using_clang_cl && !is_msvc {
            config.define("CMAKE_INTERPROCEDURAL_OPTIMIZATION", "ON");
        }

        let path = config.build();

        // ── Build-time LTO verification ────────────────────────────────
        verify_lto(&path);

        path
    } else {
        Config::new(&randomx_dir).define("DARCH", "native").build()
    };

    println!("cargo:rustc-link-search=native={}/lib64", randomx_path.display());
    println!("cargo:rustc-link-search=native={}/lib", randomx_path.display());
    println!("cargo:rustc-link-lib=static=randomx");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| "linux".to_string());
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    // Windows splits by toolchain: mingw-w64 (windows-gnu) compiles the C++
    // sources with g++, so they need libstdc++; MSVC/clang-cl use msvcrt.
    let dylib_name = match (target_os.as_str(), target_env.as_str()) {
        ("freebsd", _) | ("macos", _) | ("ios", _) => "c++", // FreeBSD, macOS and iOS use "c++"
        ("windows", "gnu") => "stdc++",                      // mingw-w64: C++ objects need libstdc++
        ("windows", _) => "msvcrt",                          // MSVC runtime on Windows (MSVC/clang-cl)
        _ => "stdc++",                                       // Default for other systems (Linux, etc.)
    };
    println!("cargo:rustc-link-lib=dylib={}", dylib_name);

    // cfg!() would test the HOST here (build.rs runs on the build machine);
    // cross-compiling from Linux to Windows must still link advapi32 for
    // RandomX's SeLockMemoryPrivilege calls. Use the target env vars instead.
    if target_os == "windows" {
        println!("cargo:rustc-link-lib=advapi32");
    }
}

/// Verify that the compiled RandomX library appears to have LTO applied.
/// We check two heuristics: (1) presence of LLVM/LTO symbols via llvm-nm, and
/// (2) binary size — a non-LTO static library is typically >3× larger than an
/// LTO'd one for RandomX (~40 MB vs ~12 MB). Logs warnings if either check fails.
fn verify_lto(build_dir: &PathBuf) {
    // Locate the compiled static library.
    let lib_path = find_static_lib(build_dir);
    let Some(lib_path) = lib_path else {
        println!("cargo:warning=LTO verification skipped — could not locate RandomX static library");
        return;
    };

    // Heuristic 1: binary size check.
    let Ok(metadata) = fs::metadata(&lib_path) else {
        println!(
            "cargo:warning=LTO verification skipped — cannot stat {}",
            lib_path.display()
        );
        return;
    };
    let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);

    // RandomX without LTO is typically ~35-45 MB; with thin LTO it's ~10-15 MB.
    if size_mb > 25.0 {
        println!(
            "cargo:warning=LTO may not be active — static library is {:.1} MB (expected <15 MB with LTO). \
             Check compiler flags and toolchain support.",
            size_mb
        );
    } else {
        println!(
            "cargo:warning=LTO verified — static library size {:.1} MB (LTO appears active)",
            size_mb
        );
    }

    // Heuristic 2: try llvm-nm to look for LTO-related symbols.
    if let Ok(output) = std::process::Command::new("llvm-nm").arg(&lib_path).output() {
        if output.status.success() {
            let nm_output = String::from_utf8_lossy(&output.stdout);
            // LTO'd libraries often contain __llvm_profile_* or thinlto-related symbols.
            let has_lto_markers = nm_output.contains("thinlto") || nm_output.contains("__llvm_profile");
            if !has_lto_markers {
                println!(
                    "cargo:warning=LTO verification: llvm-nm found no LTO markers in library — \
                     LTO may not have been applied. Size check was {:.1} MB.",
                    size_mb
                );
            }
        }
    }
    // If llvm-nm is not on PATH, silently skip this heuristic.
}

/// Walk the build directory tree to find the RandomX static library file.
fn find_static_lib(build_dir: &PathBuf) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    collect_lib_files(build_dir, &mut candidates);
    // Prefer lib64 over lib subdirectory if both exist.
    candidates.sort_by_key(|p| {
        let is_lib64 = p.parent().and_then(|p| p.file_name()) == Some(std::ffi::OsStr::new("lib64"));
        !is_lib64 as u8 // false sorts before true, so lib64 comes first
    });
    candidates.into_iter().next()
}

fn collect_lib_files(dir: &PathBuf, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_file() {
                let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if file_name.starts_with("librandomx") && (file_name.ends_with(".a") || file_name.ends_with(".o")) {
                    out.push(path);
                }
            } else if path.is_dir() {
                collect_lib_files(&path, out);
            }
        }
    }
}
