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
    let randomx_dir = env::var("RANDOMX_DIR").unwrap_or_else(|_| "RandomX".to_string());

    // Determine whether we're building with optimizations (release or opt-level > 0).
    let is_optimized = match env::var("CARGO_OPT_LEVEL") {
        Ok(level) => level != "0",
        Err(_) => false,
    };

    // ARCH (CMake variable): controls -march for helper code (AES/Argon2 init paths).
    // NOTE: this must be "ARCH", not "DARCH" — an earlier version passed DARCH, which CMake
    // silently ignored, so every cargo build actually took the portable branch below. That
    // turned out to be exactly what we want for shipping (no AVX/AVX-512 in helpers), so the
    // default stays "unset" = current production behavior: -maes plus per-file SSSE3/AVX2 for
    // the Argon2 init variants only. Set RANDOMX_ARCH=native explicitly if a local build wants
    // the build machine's full ISA in helper code (per-hash rate is unaffected either way —
    // the VM program itself is scalar SSE2 emitted by the JIT at runtime).
    let arch = env::var("RANDOMX_ARCH").unwrap_or_default();
    println!("cargo:rerun-if-env-changed=RANDOMX_ARCH");

    // A/B experiment knobs — all default to current production behavior (inert unless set):
    //   RANDOMX_DATASET_PREFETCH = nta | t2 | t1  (dataset read-ahead prefetch hint class)
    //   RANDOMX_NT_STORE_L3      = 0 | 1          (non-temporal stores for L3-region ISTOREs)
    //   RANDOMX_SPREFETCH_EARLY  = 0 | 1          (emit the scratchpad-prefetch block mid-body,
    //                                              right after the last write to either address
    //                                              register, instead of only in the epilogue —
    //                                              more lead time for the next iteration's loads)
    let prefetch = env::var("RANDOMX_DATASET_PREFETCH").unwrap_or_else(|_| "nta".to_string());
    println!("cargo:rerun-if-env-changed=RANDOMX_DATASET_PREFETCH");
    let nt_store_l3 = env::var("RANDOMX_NT_STORE_L3").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rerun-if-env-changed=RANDOMX_NT_STORE_L3");
    let sprefetch_early = env::var("RANDOMX_SPREFETCH_EARLY").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rerun-if-env-changed=RANDOMX_SPREFETCH_EARLY");

    // Extra defines for the C/C++ build. Applied to both CFLAGS and CXXFLAGS so they also
    // reach jit_compiler_x86_static.S, which is compiled as a C-language source (the cmake
    // "cheat" in RandomX/CMakeLists.txt). Prefetch hints and store hints never change
    // register state, so hash output is bit-identical regardless of the selected variant.
    let mut extra_flags = String::new();
    match prefetch.as_str() {
        "nta" => {}
        "t2" => extra_flags.push_str(" -DRANDOMX_PREFETCH_T2"),
        "t1" => extra_flags.push_str(" -DRANDOMX_PREFETCH_T1"),
        other => panic!("RANDOMX_DATASET_PREFETCH must be nta|t2|t1, got {other}"),
    }
    match nt_store_l3.as_str() {
        "0" | "" => {}
        "1" => extra_flags.push_str(" -DRANDOMX_NT_STORE_L3"),
        other => panic!("RANDOMX_NT_STORE_L3 must be 0|1, got {other}"),
    }
    match sprefetch_early.as_str() {
        "0" | "" => {}
        "1" => extra_flags.push_str(" -DRANDOMX_SPREFETCH_EARLY"),
        other => panic!("RANDOMX_SPREFETCH_EARLY must be 0|1, got {other}"),
    }

    // Build the RandomX C++ library. We pass -flto=thin via environment variables
    // because cmake's CMAKE_INTERPROCEDURAL_OPTIMIZATION is unreliable across toolchains
    // and often silently falls back to non-LTO compilation.
    let randomx_path = if is_optimized {
        let mut config = Config::new(&randomx_dir);
        if !arch.is_empty() {
            config.define("ARCH", &arch);
        }

        // Use -flto=thin via compiler flags — works with GCC, Clang, and AppleClang.
        // ThinLTO gives ~80-90% of full LTO speedup at a fraction of compile time.
        let lto_flags = format!("-flto=thin{extra_flags}");
        config.env("CFLAGS", &lto_flags)
              .env("CXXFLAGS", &lto_flags);

        // Also try cmake IPO as a secondary mechanism (works on some toolchains).
        config.define("CMAKE_INTERPROCEDURAL_OPTIMIZATION", "ON");

        let path = config.build();

        // ── Build-time LTO verification ────────────────────────────────
        verify_lto(&path);

        path
    } else {
        let mut config = Config::new(&randomx_dir);
        if !arch.is_empty() {
            config.define("ARCH", &arch);
        }
        if !extra_flags.is_empty() {
            config = config.env("CFLAGS", extra_flags.trim()).env("CXXFLAGS", extra_flags.trim());
        }
        config.build()
    };

    println!("cargo:rustc-link-search=native={}/lib64", randomx_path.display());
    println!("cargo:rustc-link-search=native={}/lib", randomx_path.display());
    println!("cargo:rustc-link-lib=static=randomx");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or("linux".to_string());
    let dylib_name = match target_os.as_str() {
        "freebsd" | "macos" | "ios" => "c++", // FreeBSD, macOS and iOS use "c++"
        "windows" => "msvcrt",                // Use MSVC runtime on Windows
        _ => "stdc++",                        // Default for other systems (Linux, etc.)
    };
    println!("cargo:rustc-link-lib=dylib={}", dylib_name);

    if cfg!(target_os = "windows") {
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
        println!("cargo:warning=LTO verification skipped — cannot stat {}", lib_path.display());
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
        println!("cargo:warning=LTO verified — static library size {:.1} MB (LTO appears active)", size_mb);
    }

    // Heuristic 2: try llvm-nm to look for LTO-related symbols.
    if let Ok(output) = std::process::Command::new("llvm-nm")
        .arg(&lib_path)
        .output()
    {
        if output.status.success() {
            let nm_output = String::from_utf8_lossy(&output.stdout);
            // LTO'd libraries often contain __llvm_profile_* or thinlto-related symbols.
            let has_lto_markers = nm_output.contains("thinlto")
                || nm_output.contains("__llvm_profile");
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
