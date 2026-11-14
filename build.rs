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
use std::path::PathBuf;

use cmake::Config;

#[allow(clippy::too_many_lines)]
fn main() {
    println!("cargo:rerun-if-env-changed=RANDOMX_MSVC_CXX");
    println!("cargo:rerun-if-env-changed=LLVM_INSTALL_DIR");
    println!("cargo:rerun-if-env-changed=RANDOMX_ARCH");
    let randomx_dir = env::var("RANDOMX_DIR").unwrap_or_else(|_| "RandomX".to_string());

    // Resolve the RandomX CMake ARCH setting. RandomX's CMakeLists reads
    // ${ARCH} (NOT DARCH — a -DDARCH define is dead and silently ignored) and
    // uses it as -march=${ARCH} for the whole C++ support layer on GNU/Clang
    // toolchains; the MSVC branch never consults ARCH at all. Without this,
    // mingw (windows-gnu) builds compile the support layer at baseline
    // x86-64 — no BMI2/FMA/AVX2 — which costs real per-hash throughput.
    //
    // Default: "native" ONLY for windows-gnu builds (they are built per-box
    // on the machine that mines, so a CPU-specific binary is fine). Linux
    // keeps "default" — universal/glibc-pinned builds (cargo zigbuild) must
    // stay portable across machines. RANDOMX_ARCH overrides both.
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| "linux".to_string());
    let arch = match env::var("RANDOMX_ARCH") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            if target_os == "windows" && target_env == "gnu" {
                "native".to_string()
            } else {
                "default".to_string()
            }
        }
    };
    println!(
        "cargo:warning=randomx-rs: ARCH={arch} (target {target_os}/{target_env}; set RANDOMX_ARCH to override)"
    );

    // Resolve the MSVC-side C/C++ compiler. RANDOMX_MSVC_CXX is an explicit
    // override: a path to clang-cl.exe selects it; "0", "cl", "false" or empty
    // forces the stock cl.exe. When unset on optimized msvc builds we
    // auto-detect clang-cl: cl.exe codegen for the RandomX C++ support layer
    // is ~60% slower per-hash than clang-cl (measured 434 vs 740 H/s per VM
    // on a Ryzen 3600), so auto-detection gives local builders the fast path
    // with zero configuration.
    let clang_cl = match env::var("RANDOMX_MSVC_CXX") {
        Ok(v) => {
            let v = v.trim().to_string();
            if v.is_empty()
                || v == "0"
                || v.eq_ignore_ascii_case("cl")
                || v.eq_ignore_ascii_case("false")
            {
                None
            } else {
                Some(v)
            }
        }
        Err(_) => find_clang_cl(),
    };

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
        if clang_cl.is_some() {
            let out = env::var("OUT_DIR").expect("OUT_DIR");
            let build_dir = std::path::Path::new(&out).join("build");
            if build_dir.exists() {
                println!("cargo:warning=randomx-rs: wiping cached CMake build tree (clang-cl compiler active)");
                std::fs::remove_dir_all(&build_dir).expect("failed to wipe cached CMake build dir");
            }
        }
        let mut config = Config::new(&randomx_dir);
        config.define("ARCH", &arch);

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
        if let Some(ref cxx) = clang_cl {
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
            // clang-cl does NOT imply AES from /arch:AVX2 the way cl.exe does,
            // so intrinsics like _mm_aesenc_si128 fail with "requires target
            // feature 'aes'". /clang:-X passes raw LLVM flags through clang-cl.
            config.cflag("/clang:-maes");
            config.cxxflag("/clang:-maes");
            // clang-cl leaves C++ exceptions off in this configuration
            // (RandomX's InterpretedVm::operator new throws std::bad_alloc).
            config.cflag("/EHsc");
            config.cxxflag("/EHsc");
        }
        if env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            config.cflag("/arch:AVX2");
            config.cxxflag("/arch:AVX2");
        }

        // LTO/IPO deliberately OFF everywhere. Every mechanism tried (env
        // -flto=thin and CMAKE_INTERPROCEDURAL_OPTIMIZATION) emits LTO
        // bytecode into librandomx.a that the final rustc-driven link cannot
        // consume: MSVC link.exe -> LNK2019 unresolved randomx_*, and Linux
        // rust-lld/collect2 -> undefined randomx_* against GCC LTO objects.
        // Measured hashrates (mingw ~6.72 KH/s, clang-cl 6.74 KH/s, Linux)
        // were all achieved without LTO/IPO; it buys nothing here.
        let using_clang_cl = clang_cl.is_some()
            && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");

        if clang_cl.is_none()
            && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
        {
            println!("cargo:warning=randomx-rs: clang-cl not found; using cl.exe (measured ~60% slower RandomX codegen). Install LLVM (winget install LLVM.LLVM), run cargo clean -p randomx-rs, and rebuild -- or set RANDOMX_MSVC_CXX=<path to clang-cl.exe> to override.");
        }

        let path = config.build();

        // Surface the compiler that actually configured the CMake build —
        // an env-gated branch that "ran" but was defeated by a cached
        // CMakeCache (or a generator that ignores -D compiler defines) is
        // otherwise invisible: the build succeeds with the WRONG compiler.
        // Read CMakeCache.txt instead of trusting the -D defines we passed.
        // cmake-rs's build() returns the INSTALL dir; the CMake build tree
        // (holding CMakeCache.txt) sits under <install>/out/build.
        let cache = std::path::Path::new(&path)
            .join("out")
            .join("build")
            .join("CMakeCache.txt");
        if let Ok(text) = std::fs::read_to_string(&cache) {
            for key in ["CMAKE_CXX_COMPILER:FILEPATH", "CMAKE_C_COMPILER:FILEPATH"] {
                for line in text.lines() {
                    if line.starts_with(key) {
                        println!("cargo:warning=randomx-rs: {}", line.trim());
                        break;
                    }
                }
            }
        }

        path
    } else {
        Config::new(&randomx_dir)
            .define("ARCH", &arch)
            .build()
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

/// Locate clang-cl.exe without any configuration: PATH first, then the
/// standard LLVM install locations. Returns None on non-msvc targets so
/// Linux/mingw builds are never touched by auto-detection.
fn find_clang_cl() -> Option<String> {
    if env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return None;
    }

    let mut candidates: Vec<PathBuf> = Vec::new();

    // 1. PATH lookup (works when LLVM is on PATH or via VS integration).
    if let Ok(path_var) = env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            candidates.push(dir.join("clang-cl.exe"));
        }
    }

    // 2. LLVM_INSTALL_DIR hint.
    if let Ok(dir) = env::var("LLVM_INSTALL_DIR") {
        candidates.push(PathBuf::from(dir).join("bin").join("clang-cl.exe"));
    }

    // 3. Standard install roots.
    for var in ["ProgramFiles", "ProgramFiles(x86)", "LocalAppData"] {
        if let Ok(root) = env::var(var) {
            candidates.push(PathBuf::from(root).join("LLVM").join("bin").join("clang-cl.exe"));
        }
    }

    candidates
        .into_iter()
        .find(|c| c.exists())
        .and_then(|c| c.to_str().map(|s| s.to_string()))
}
