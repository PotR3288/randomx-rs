![CI](https://github.com/tari-project/randomx-rs/actions/workflows/ci.yml/badge.svg)
[![Coverage Status](https://coveralls.io/repos/github/delta1/randomx-rs/badge.svg?branch=cov-temp)](https://coveralls.io/github/delta1/randomx-rs?branch=cov-temp)

# RandomX-rs

> Rust bindings to the RandomX proof-of-work (Pow) system

## Build Dependencies

This repo makes use of git submodules.

The first time you compile, or perhaps after a big update after a `git pull`, you need to update the submodules:

```bash
git submodule init
git submodule update
```

If you see an error like

```
fatal: Needed a single revision
Unable to find current revision in submodule path 'RandomX'
```

you might want to see if there is a `RandomX` folder in the source tree. (On case insensitive systems, like OsX and Windows, it might
even be `randomx`). Deleting this folder and repeating the commands above should resolve the issue.

### Mac

Install [XCode](https://apps.apple.com/za/app/xcode/id497799835?mt=12) and then the XCode Command Line Tools with the following command

```
xcode-select --install
```

For macOS Mojave additional headers need to be installed, run

```
open /Library/Developer/CommandLineTools/Packages/macOS_SDK_headers_for_macOS_10.14.pkg
```

and follow the prompts

Install Brew

```
/usr/bin/ruby -e "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/master/install)"
```

Run the following to install needed bottles

```
brew install git
brew install cmake
```

### Linux

Run the following to install dependencies

```
apt-get install git cmake libc++-dev libc++abi-dev
```

### Windows

Install [Git](https://git-scm.com/download/win)

Install [CMake](https://cmake.org/download/)

Install [Build Tools for Visual Studio 2019](https://visualstudio.microsoft.com/thank-you-downloading-visual-studio/?sku=BuildTools&rel=16)

### Android

To build using the Android NDK the `ANDROID_SDK_ROOT` environment variable needs to be set. Other variables are optional as they have defaults. Example build command for ARM64:
```
ANDROID_SDK_ROOT=/home/user/Android/Sdk \
ANDROID_PLATFORM=android-25 \
ANDROID_CMAKE=/home/user/Android/Sdk/cmake/3.22.1/bin/cmake \
ANDROID_CMAKE_TOOLCHAIN=/home/user/Android/Sdk/ndk/22.1.7171670/build/cmake/android.toolchain.cmake \
cargo build --target=aarch64-linux-android
```

# Troubleshooting

## Mac/OSX

If you're experiencing linker issues, or messages like

`cstdint:153:8: error: no member named 'int8_t' in the global namespace`

then you might have multiple conflicting versions of clang installed.

Try:

- Does `which cc` report more than one binary? If so, uninstalling one of the clang compilers might help.
- Upgrading cmake. `brew uninstall cmake && brew install cmake`
- `cargo clean`

On Apple ARM64 hardware and newer XCode releases, RandomX might fail the `randomx-tests`.
```
[83] Hash test 1e (interpreter)               ... PASSED
[84] Hash test 2a (compiler)                  ... Assertion failed: (equalsHex(hash, "639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f")), function operator(), file tests.cpp, line 966.
zsh: abort      ./randomx-tests
```
or
```
[88] Hash test 2e (compiler)                  ... PASSED
[89] Cache initialization: SSSE3              ... SKIPPED
[90] Cache initialization: AVX2               ... SKIPPED
[91] Hash batch test                          ... Assertion failed: (equalsHex(hash3, "c36d4ed4191e617309867ed66a443be4075014e2b061bcdaf9ce7b721d2b77a8")), function operator(), file tests.cpp, line 1074.
zsh: abort      ./randomx-tests
```
 Building using an older SDK might help. Find location of current SDKs with `xcrun --show-sdk-path`, then for example:
```bash
export RANDOMX_RS_CMAKE_OSX_SYSROOT="/Library/Developer/CommandLineTools/SDKs/MacOSX12.3.sdk"
cargo build
```
Quick test with built binaries
```bash
find target -name randomx-tests -exec {} \;
```

## About this fork

This is a fork of [tari-project/randomx-rs](https://github.com/tari-project/randomx-rs), based on the `development` branch at v1.6.0 (RandomX C++ v1.2.3). The public API stays compatible with upstream — code written against the crates.io crate compiles unchanged, and nothing existing was renamed or re-signatured. What follows is what this fork adds, changes, and removes, in plain terms.

### Added

**`Send`/`Sync` for datasets and VMs.** The upstream types cannot be moved to another thread or shared between threads, even though the underlying C library is safe under exactly that usage: each `RandomXVM` owns its own state, and a dataset is read-only once initialized. This fork adds `unsafe impl Send/Sync`, so a miner can hand every worker thread its own VM while they all share one dataset — no wrapper types needed.

**Parallel dataset initialization.** Building the ~2 GiB dataset single-threaded takes tens of seconds at startup. The new `RandomXDataset::new_uninitialized()` allocates the memory without initializing it, and `init_range(start, count)` initializes a slice of it; disjoint slices can be initialized concurrently from multiple threads (the same scheme xmrig uses to parallelize dataset generation). `reinit_with_cache()` re-keys an existing dataset for a new job without reallocating. The original `new(flags, cache, start)` is now implemented on top of these and behaves exactly as before.

**Per-NUMA dataset replicas.** On multi-socket or multi-die machines, a worker that reads a 2 GiB dataset living in another NUMA node's memory pays an interconnect penalty on every access. `copy_from()` (with the `raw_memory()`/`memory_size()` helpers) lets you memcpy a fully initialized dataset into a second allocation so each NUMA node holds its own local copy, and workers can be pinned to their node's copy.

**Win32 error codes from large-page allocation.** When the 2 GiB large-page allocation fails on Windows, upstream only says "Could not allocate dataset". The new `randomx_alloc_dataset_ex()` C function (exposed in Rust as `new_uninitialized_with_err()`, Windows-only) also reports the Win32 error code — e.g. 1450 (`ERROR_NOT_ENOUGH_MEMORY`) — so you can tell RAM exhaustion from a hugepage configuration problem instead of guessing.

### Changed

**The RandomX C++ source is now vendored in this repo.** Upstream keeps it as a git submodule; here the tree is committed directly and `.gitmodules` was removed. The fork needed its own patched C layer (below), and a submodule pin can only point at commits that exist on a remote — so the tree was copied in to make the fork self-contained: `cargo build` works from any checkout or rev pin without resolving submodules, and future C-layer patches are ordinary commits here.

**Linux huge pages prefer 1 GiB.** The vendored `virtual_memory.c` now tries `MAP_HUGETLB | MAP_HUGE_1GB` first when allocating the dataset's large pages, falling back to the default 2 MiB size if 1 GiB pages are not reserved on the system. A ~2 GiB dataset as one or two 1 GiB mappings means fewer TLB misses; behavior is unchanged on stock systems because of the fallback.

**Windows builds now produce competitive hashrates.** Upstream's `build.rs` compiles the RandomX C++ support layer with MSVC defaults — baseline x86-64, no AVX2 — which measured ~434 H/s per VM on a Ryzen 3600. This fork:

- passes `/arch:AVX2` to the C/C++ compiler for msvc targets;
- auto-detects `clang-cl.exe` (LLVM) and uses it as the MSVC-side compiler when building with optimizations, because cl.exe's codegen of this layer is ~60% slower per hash than clang-cl's (~740 H/s/VM vs 434); a full 12-VM build now matches mingw (6.74 vs 6.72 KH/s). `RANDOMX_MSVC_CXX` overrides the choice: a path pins a specific compiler, and `0` forces stock cl.exe;
- adds the flags clang-cl needs that cl.exe implies (`/clang:-maes` for AES-NI, `/EHsc` because RandomX throws `std::bad_alloc`), forces the Ninja generator so those defines are honored, and wipes stale CMake caches when the compiler switches;
- fixes cross-compilation: `advapi32` is linked based on the *target* OS rather than the host's, and windows-gnu (mingw) targets link libstdc++.

LTO/IPO was tried and then removed entirely — every mechanism emits objects that MSVC's `link.exe` (and Linux rust-lld against GCC LTO objects) cannot consume, and measured hashrates were identical without it. The crate's own `[profile.release]` now sets `lto = "fat"`, `opt-level = 3`, `codegen-units = 1` for standalone builds of this repo; when consumed as a dependency the top-level workspace profile applies instead.

### Removed

- The git submodule workflow for the RandomX C++ tree (see above).
- An internal prefix-zeroing workaround in dataset creation that predated `init_range` — range initialization handles uninitialised memory properly now.
- A/B experiment knobs for prefetch hints and L3 non-temporal stores were added during development and reverted; no API or flags from them remain.
 