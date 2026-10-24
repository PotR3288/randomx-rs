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

//! # RandomX
//!
//! The `randomx-rs` crate provides bindings to the RandomX proof-of-work (PoW) system.
//!
//! From the [RandomX github repo]:
//!
//! "RandomX is a proof-of-work (PoW) algorithm that is optimized for general-purpose CPUs. RandomX uses random code
//! execution together with several memory-hard techniques to minimize the efficiency advantage of specialized
//! hardware."
//!
//! Read more about how RandomX works in the [design document].
//!
//! [RandomX github repo]: <https://github.com/tevador/RandomX>
//! [design document]: <https://github.com/tevador/RandomX/blob/master/doc/design.md>
mod bindings;
/// Test utilities for fuzzing
pub mod test_utils;

use std::{convert::TryFrom, num::TryFromIntError, ptr, sync::{Arc, Mutex}};

use bindings::{
    randomx_alloc_cache,
    randomx_alloc_dataset,
    randomx_cache,
    randomx_calculate_hash,
    randomx_create_vm,
    randomx_dataset,
    randomx_dataset_item_count,
    randomx_destroy_vm,
    randomx_get_dataset_memory,
    randomx_init_cache,
    randomx_init_dataset,
    randomx_release_cache,
    randomx_release_dataset,
    randomx_vm,
    randomx_vm_set_cache,
    randomx_vm_set_dataset,
    RANDOMX_HASH_SIZE,
};
#[cfg(windows)]
use bindings::randomx_alloc_dataset_ex;
use bitflags::bitflags;
use libc::{c_ulong, c_void};
use thiserror::Error;

/// The size, in bytes, of a single RandomX dataset item. The full dataset returned by
/// [`RandomXDataset::get_data`] is `RandomXDataset::count()` items of this size.
pub use crate::bindings::RANDOMX_DATASET_ITEM_SIZE;
use crate::bindings::{
    randomx_calculate_hash_first,
    randomx_calculate_hash_last,
    randomx_calculate_hash_next,
    randomx_get_flags,
};

bitflags! {
    /// RandomX Flags are used to configure the library.
    pub struct RandomXFlag: u32 {
        /// No flags set. Works on all platforms, but is the slowest.
        const FLAG_DEFAULT      = 0b0000_0000;
        /// Allocate memory in large pages.
        const FLAG_LARGE_PAGES  = 0b0000_0001;
        /// Use hardware accelerated AES.
        const FLAG_HARD_AES     = 0b0000_0010;
        /// Use the full dataset.
        const FLAG_FULL_MEM     = 0b0000_0100;
        /// Use JIT compilation support.
        const FLAG_JIT          = 0b0000_1000;
        /// When combined with FLAG_JIT, the JIT pages are never writable and executable at the
        /// same time.
        const FLAG_SECURE       = 0b0001_0000;
        /// Optimize Argon2 for CPUs with the SSSE3 instruction set.
        const FLAG_ARGON2_SSSE3 = 0b0010_0000;
        /// Optimize Argon2 for CPUs with the AVX2 instruction set.
        const FLAG_ARGON2_AVX2  = 0b0100_0000;
        /// Optimize Argon2 for CPUs without the AVX2 or SSSE3 instruction sets.
        const FLAG_ARGON2       = 0b0110_0000;
    }
}

impl RandomXFlag {
    /// Returns the recommended flags to be used.
    ///
    /// Does not include:
    /// * FLAG_LARGE_PAGES
    /// * FLAG_FULL_MEM
    /// * FLAG_SECURE
    ///
    /// The above flags need to be set manually, if required.
    pub fn get_recommended_flags() -> RandomXFlag {
        RandomXFlag {
            bits: unsafe { randomx_get_flags() },
        }
    }
}

impl Default for RandomXFlag {
    /// Default value for RandomXFlag
    fn default() -> RandomXFlag {
        RandomXFlag::FLAG_DEFAULT
    }
}

#[derive(Debug, Clone, Error)]
/// This enum specifies the possible errors that may occur.
pub enum RandomXError {
    #[error("Problem creating the RandomX object: {0}")]
    CreationError(String),
    #[error("Problem with configuration flags: {0}")]
    FlagConfigError(String),
    #[error("Problem with parameters supplied: {0}")]
    ParameterError(String),
    #[error("Failed to convert Int to usize")]
    TryFromIntError(#[from] TryFromIntError),
    #[error("Unknown problem running RandomX: {0}")]
    Other(String),
}

#[derive(Debug)]
struct RandomXCacheInner {
    cache_ptr: *mut randomx_cache,
}

impl Drop for RandomXCacheInner {
    /// De-allocates memory for the `cache` object
    fn drop(&mut self) {
        unsafe {
            randomx_release_cache(self.cache_ptr);
        }
    }
}

#[derive(Debug, Clone)]
/// The Cache is used for light verification and Dataset construction.
pub struct RandomXCache {
    inner: Arc<RandomXCacheInner>,
}

impl RandomXCache {
    /// Creates and alllcates memory for a new cache object, and initializes it with
    /// the key value.
    ///
    /// `flags` is any combination of the following two flags:
    /// * FLAG_LARGE_PAGES
    /// * FLAG_JIT
    ///
    /// and (optionally) one of the following flags (depending on instruction set supported):
    /// * FLAG_ARGON2_SSSE3
    /// * FLAG_ARGON2_AVX2
    ///
    /// `key` is a sequence of u8 used to initialize SuperScalarHash.
    pub fn new(flags: RandomXFlag, key: &[u8]) -> Result<RandomXCache, RandomXError> {
        if key.is_empty() {
            Err(RandomXError::ParameterError("key is empty".to_string()))
        } else {
            let cache_ptr = unsafe { randomx_alloc_cache(flags.bits) };
            if cache_ptr.is_null() {
                Err(RandomXError::CreationError("Could not allocate cache".to_string()))
            } else {
                let inner = RandomXCacheInner { cache_ptr };
                let result = RandomXCache { inner: Arc::new(inner) };
                let key_ptr = key.as_ptr() as *mut c_void;
                let key_size = key.len();
                unsafe {
                    randomx_init_cache(result.inner.cache_ptr, key_ptr, key_size);
                }
                Ok(result)
            }
        }
    }
}

#[derive(Debug)]
struct RandomXDatasetInner {
    dataset_ptr: *mut randomx_dataset,
    dataset_count: u32,
    /// The cache the dataset was last (re)initialized from. Kept alive for the
    /// dataset's lifetime; swapped by `reinit_with_cache` on seed change.
    /// Guarded so the swap is atomic across threads; the lock is held only
    /// briefly (clone or swap), never across a dataset-init FFI call.
    #[allow(dead_code)]
    cache: Mutex<RandomXCache>,
}

impl Drop for RandomXDatasetInner {
    /// De-allocates memory for the `dataset` object.
    fn drop(&mut self) {
        unsafe {
            randomx_release_dataset(self.dataset_ptr);
        }
    }
}

// The dataset is read-only after initialization — threads only read from it during hashing, never write.
// Safe to share across threads with no locking needed.
unsafe impl Send for RandomXDatasetInner {}
unsafe impl Sync for RandomXDatasetInner {}

#[derive(Debug, Clone)]
/// The Dataset is a read-only memory structure that is used during VM program execution.
pub struct RandomXDataset {
    inner: Arc<RandomXDatasetInner>,
}

impl RandomXDataset {
    /// Creates a new dataset object, allocates memory to the `dataset` object and initializes it.
    ///
    /// `flags` is one of the following:
    /// * FLAG_DEFAULT
    /// * FLAG_LARGE_PAGES
    ///
    /// `cache` is a cache object.
    ///
    /// `start` is the item number where initialization should start. **Pass 0.** The items
    /// `[start, RandomXDataset::count())` are initialized by the RandomX library; the leading `start` items are
    /// left undefined — the library never writes them and this wrapper does not zero them either (freshly
    /// allocated pages happen to read as zero on most platforms, but that is an allocator detail, not a contract).
    ///
    /// # Warning
    ///
    /// The RandomX API requires that *every* item from `0` to `RandomXDataset::count() - 1` is initialized before a
    /// dataset may be used (see the note on `randomx_init_dataset` in `randomx.h`). A non-zero `start` therefore
    /// produces a dataset that does **not** satisfy that precondition and must **not** be passed to
    /// [`RandomXVM::new`] or [`RandomXVM::reinit_dataset`]. Doing so is not detected or reported: the VM will read
    /// the uninitialised leading items as though they were real dataset items and silently compute hashes that disagree
    /// with every other RandomX implementation.
    ///
    /// The only legitimate use of a non-zero `start` in the upstream API is splitting the initialization of a single
    /// *shared* dataset across several threads, each initializing a different item range. This wrapper cannot express
    /// that, because `new` always allocates its own dataset, so there is no correct value other than 0.
    ///
    /// This fork *can* express it: [`RandomXDataset::new_uninitialized`] plus
    /// [`RandomXDataset::init_range`] initialize disjoint ranges of one shared dataset from multiple threads.
    // Conversions may be lossy on Windows or Linux
    #[allow(clippy::useless_conversion)]
    pub fn new(flags: RandomXFlag, cache: RandomXCache, start: u32) -> Result<RandomXDataset, RandomXError> {
        let dataset = Self::new_uninitialized(flags, cache)?;
        let item_count = dataset.inner.dataset_count;
        if start >= item_count {
            return Err(RandomXError::CreationError(format!(
                "start must be less than item_count: start: {start}, item_count: {item_count}",
            )));
        }
        dataset.init_range(start, item_count - start)?;
        Ok(dataset)
    }

    /// Creates a new dataset object and allocates its memory buffer without
    /// initializing any items.
    ///
    /// Initialize the item ranges afterwards with [`RandomXDataset::init_range`]
    /// — a single call for the whole range, or one call per slice from multiple
    /// threads (disjoint slices are safe to initialize concurrently, which is
    /// how xmrig parallelizes dataset generation).
    ///
    /// `flags` is one of the following:
    /// * FLAG_DEFAULT
    /// * FLAG_LARGE_PAGES
    ///
    /// `cache` is a cache object (must be initialized).
    pub fn new_uninitialized(flags: RandomXFlag, cache: RandomXCache) -> Result<RandomXDataset, RandomXError> {
        let item_count = RandomXDataset::count()
            .map_err(|e| RandomXError::CreationError(format!("Could not get dataset count: {e:?}")))?;

        let dataset_ptr = unsafe { randomx_alloc_dataset(flags.bits) };
        if dataset_ptr.is_null() {
            Err(RandomXError::CreationError("Could not allocate dataset".to_string()))
        } else {
            let inner = RandomXDatasetInner {
                dataset_ptr,
                dataset_count: item_count,
                cache: Mutex::new(cache),
            };
            Ok(RandomXDataset { inner: Arc::new(inner) })
        }
    }

#[cfg(windows)]
    /// Allocate a dataset buffer without initializing its items. Like [`new_uninitialized`], but when
    /// `FLAG_LARGE_PAGES` is set and allocation fails, returns the Win32 error code (e.g. 1450/1455
    /// = insufficient physical RAM) in addition to the generic failure — useful for diagnostic logging
    /// on Windows where huge-page commit failures are often resource-pressure related rather than a
    /// privilege or configuration problem.
    ///
    /// On non-Windows platforms `win32_error` is always set to 0.
    pub fn new_uninitialized_with_err(
        flags: RandomXFlag,
        cache: RandomXCache,
    ) -> Result<(RandomXDataset, Option<u32>), RandomXError> {
        let item_count = RandomXDataset::count()
            .map_err(|e| RandomXError::CreationError(format!("Could not get dataset count: {e:?}")))?;

        let mut win32_err: u32 = 0;
        let dataset_ptr = unsafe { randomx_alloc_dataset_ex(flags.bits(), &mut win32_err) };
        if dataset_ptr.is_null() {
            if win32_err != 0 {
                Err(RandomXError::CreationError(
                    format!("Could not allocate dataset (Win32 error: {})", win32_err),
                ))
            } else {
                Err(RandomXError::CreationError("Could not allocate dataset".to_string()))
            }
        } else {
            let inner = RandomXDatasetInner {
                dataset_ptr,
                dataset_count: item_count,
                cache: Mutex::new(cache),
            };
            Ok((RandomXDataset { inner: Arc::new(inner) }, if win32_err != 0 { Some(win32_err) } else { None }))
        }
    }

    /// Initializes the item range `[start, start + count)` of the dataset.
    ///
    /// The range must lie within `[0, item_count]` (see [`RandomXDataset::count`]).
    /// Disjoint ranges may be initialized concurrently from multiple threads:
    /// each 64-byte item is written by exactly one slice and the cache is only
    /// read during initialization.
    pub fn init_range(&self, start: u32, count: u32) -> Result<(), RandomXError> {
        let item_count = self.inner.dataset_count;
        if start >= item_count || count > item_count - start {
            return Err(RandomXError::ParameterError(format!(
                "init_range out of bounds: start: {start}, count: {count}, item_count: {item_count}",
            )));
        }
        if count == 0 {
            return Ok(());
        }
        // Clone the Arc (cheap) and drop the lock before the FFI call so that
        // concurrent init_range calls on disjoint slices are never serialized.
        let cache = self.inner.cache.lock().unwrap().clone();
        unsafe {
            randomx_init_dataset(
                self.inner.dataset_ptr,
                cache.inner.cache_ptr,
                c_ulong::from(start),
                c_ulong::from(count),
            );
        }
        Ok(())
    }

    /// Switches the dataset to a new cache (seed change) so that subsequent
    /// [`RandomXDataset::init_range`] calls regenerate items from the new
    /// seed. The dataset's memory buffer is NOT reallocated — the caller
    /// refills it in place, which is what keeps the dataset pointer (and any
    /// VMs linked to it) stable across seed changes, mirroring xmrig.
    pub fn reinit_with_cache(&self, new_cache: &RandomXCache) -> Result<(), RandomXError> {
        *self.inner.cache.lock().unwrap() = new_cache.clone();
        Ok(())
    }

    /// Returns the number of items in the `dataset` or an error on failure.
    pub fn count() -> Result<u32, RandomXError> {
        match unsafe { randomx_dataset_item_count() } {
            0 => Err(RandomXError::Other("Dataset item count was 0".to_string())),
            x => {
                // This weirdness brought to you by c_ulong being different on Windows and Linux
                #[cfg(target_os = "windows")]
                return Ok(x);
                #[cfg(not(target_os = "windows"))]
                return Ok(u32::try_from(x)?);
            },
        }
    }

    /// Returns a copy of the *entire* internal memory buffer of the `dataset`, or an error on failure.
    ///
    /// The returned buffer is `RandomXDataset::count()` items of [`RANDOMX_DATASET_ITEM_SIZE`] (64) bytes each, i.e.
    /// approximately 2.03 GB with the default RandomX configuration. This is an expensive, fully allocating copy of
    /// the dataset, so avoid calling it on a hot path. The allocation is fallible: an out-of-memory condition is
    /// reported as a [`RandomXError`] instead of aborting the process.
    ///
    /// If the dataset was created with a non-zero `start`, the first `start` items were never initialised by the
    /// RandomX library; cover `[0, start)` with [`RandomXDataset::init_range`] before using or reading the dataset,
    /// otherwise those bytes are undefined.
    pub fn get_data(&self) -> Result<Vec<u8>, RandomXError> {
        let memory = unsafe { randomx_get_dataset_memory(self.inner.dataset_ptr) };
        if memory.is_null() {
            return Err(RandomXError::Other("Could not get dataset memory".into()));
        }
        // `dataset_count` is an *item* count, not a byte count; each item is `RANDOMX_DATASET_ITEM_SIZE` bytes.
        let item_count = usize::try_from(self.inner.dataset_count)?;
        let size_in_bytes = item_count.checked_mul(RANDOMX_DATASET_ITEM_SIZE).ok_or_else(|| {
            RandomXError::Other(format!(
                "Dataset size overflows usize: {item_count} items of {RANDOMX_DATASET_ITEM_SIZE} bytes each",
            ))
        })?;
        // SAFETY: `memory` is a non-null pointer to the dataset buffer owned by the RandomX library. The library
        // allocated that buffer with room for `randomx_dataset_item_count()` items of `RANDOMX_DATASET_ITEM_SIZE`
        // bytes each, and `dataset_count` was set from that same call, so exactly `size_in_bytes` bytes are inside
        // the allocation. Every one of those bytes is initialised provided the dataset was fully initialised —
        // either by [`RandomXDataset::new`] (which has the library write the items `[start, count)`, so pass 0)
        // or by covering all of `[0, count)` with [`RandomXDataset::init_range`]. `u8` has an
        // alignment of 1, so the pointer is trivially aligned. The buffer outlives the slice: `&self` keeps the
        // `Arc<RandomXDatasetInner>` (and hence the dataset allocation) alive, the dataset is read-only once
        // initialised, and the slice is copied into an owned `Vec` before this function returns.
        let data = unsafe { std::slice::from_raw_parts(memory.cast::<u8>(), size_in_bytes) };
        // Allocate fallibly: a plain `to_vec` of ~2 GB would call `handle_alloc_error` and abort the whole process
        // on failure, which is not an acceptable outcome for a library that returns a `Result`.
        let mut result = Vec::new();
        result.try_reserve_exact(size_in_bytes).map_err(|e| {
            RandomXError::Other(format!(
                "Could not allocate {size_in_bytes} bytes for the dataset copy: {e}"
            ))
        })?;
        result.extend_from_slice(data);
        Ok(result)
    }

    /// Returns a raw pointer to the internal memory buffer of the `dataset`.
    ///
    /// The buffer is [`RandomXDataset::memory_size`] bytes long and stays valid
    /// for as long as this dataset (or any clone sharing its `Arc`) is alive.
    /// Treat the contents as opaque — the layout is RandomX-internal.
    pub fn raw_memory(&self) -> *mut u8 {
        unsafe { randomx_get_dataset_memory(self.inner.dataset_ptr) as *mut u8 }
    }

    /// Returns the byte size of the internal memory buffer
    /// (`item_count * RANDOMX_DATASET_ITEM_SIZE` — the full dataset size,
    /// 2080 MiB with the Tari RandomX configuration).
    pub fn memory_size(&self) -> usize {
        self.inner.dataset_count as usize * 64
    }

    /// Copies the entire internal memory buffer from another dataset into this one.
    ///
    /// Both datasets must hold the same item count (fixed by the RandomX
    /// configuration, so this holds for any two datasets of the same build) and
    /// the source must already be fully initialized. The buffer pointer of the
    /// destination is unchanged — only its contents are replaced — so VMs linked
    /// to it keep pointing at valid memory. This is the same scheme xmrig uses
    /// to fill per-NUMA-node dataset replicas (`RxDataset::setRaw`).
    pub fn copy_from(&self, src: &RandomXDataset) -> Result<(), RandomXError> {
        if self.inner.dataset_count != src.inner.dataset_count {
            return Err(RandomXError::ParameterError(format!(
                "copy_from: item count mismatch: {} != {}",
                self.inner.dataset_count, src.inner.dataset_count,
            )));
        }
        let size = self.memory_size();
        if size == 0 {
            return Err(RandomXError::ParameterError("copy_from: empty dataset".to_string()));
        }
        let dst = self.raw_memory();
        let source = src.raw_memory();
        if dst.is_null() || source.is_null() {
            return Err(RandomXError::Other("copy_from: null dataset memory".to_string()));
        }
        unsafe {
            libc::memcpy(dst as *mut c_void, source as *const c_void, size);
        }
        Ok(())
    }
}

#[derive(Debug)]
/// The RandomX Virtual Machine (VM) is a complex instruction set computer that executes generated programs.
pub struct RandomXVM {
    flags: RandomXFlag,
    vm: *mut randomx_vm,
    linked_cache: Option<RandomXCache>,
    linked_dataset: Option<RandomXDataset>,
}

// The VM pointer is per-thread and safe to share behind a Mutex.
unsafe impl Send for RandomXVM {}
unsafe impl Sync for RandomXVM {}

impl Drop for RandomXVM {
    /// De-allocates memory for the `VM` object.
    fn drop(&mut self) {
        unsafe {
            randomx_destroy_vm(self.vm);
        }
    }
}

impl RandomXVM {
    /// Creates a new `VM` and initializes it, error on failure.
    ///
    /// `flags` is any combination of the following 5 flags:
    /// * FLAG_LARGE_PAGES
    /// * FLAG_HARD_AES
    /// * FLAG_FULL_MEM
    /// * FLAG_JIT
    /// * FLAG_SECURE
    ///
    /// Or
    ///
    /// * FLAG_DEFAULT
    ///
    /// `cache` is a cache object, optional if FLAG_FULL_MEM is set.
    ///
    /// `dataset` is a dataset object, optional if FLAG_FULL_MEM is not set.
    pub fn new(
        flags: RandomXFlag,
        cache: Option<RandomXCache>,
        dataset: Option<RandomXDataset>,
    ) -> Result<RandomXVM, RandomXError> {
        let is_full_mem = flags.contains(RandomXFlag::FLAG_FULL_MEM);
        match (cache, dataset) {
            (None, None) => Err(RandomXError::CreationError("Failed to allocate VM".to_string())),
            (None, _) if !is_full_mem => Err(RandomXError::FlagConfigError(
                "No cache and FLAG_FULL_MEM not set".to_string(),
            )),
            (_, None) if is_full_mem => Err(RandomXError::FlagConfigError(
                "No dataset and FLAG_FULL_MEM set".to_string(),
            )),
            (cache, dataset) => {
                let cache_ptr = cache
                    .as_ref()
                    .map(|stash| stash.inner.cache_ptr)
                    .unwrap_or_else(ptr::null_mut);
                let dataset_ptr = dataset
                    .as_ref()
                    .map(|data| data.inner.dataset_ptr)
                    .unwrap_or_else(ptr::null_mut);
                let vm = unsafe { randomx_create_vm(flags.bits, cache_ptr, dataset_ptr) };
                if vm.is_null() {
                    // The C layer returns null when the VM (or its JIT code
                    // buffer) cannot be allocated — e.g. OOM or a denied RWX
                    // mmap. Surfacing this as an error instead of wrapping a
                    // null pointer avoids a segfault on the first hash.
                    return Err(RandomXError::CreationError("Could not allocate VM".to_string()));
                }
                Ok(RandomXVM {
                    vm,
                    flags,
                    linked_cache: cache,
                    linked_dataset: dataset,
                })
            },
        }
    }

    /// Re-initializes the `VM` with a new cache that was initialised without
    /// RandomXFlag::FLAG_FULL_MEM.
    pub fn reinit_cache(&mut self, cache: RandomXCache) -> Result<(), RandomXError> {
        if self.flags.contains(RandomXFlag::FLAG_FULL_MEM) {
            Err(RandomXError::FlagConfigError(
                "Cannot reinit cache with FLAG_FULL_MEM set".to_string(),
            ))
        } else {
            unsafe {
                randomx_vm_set_cache(self.vm, cache.inner.cache_ptr);
            }
            self.linked_cache = Some(cache);
            Ok(())
        }
    }

    /// Re-initializes the `VM` with a new dataset that was initialised with
    /// RandomXFlag::FLAG_FULL_MEM.
    pub fn reinit_dataset(&mut self, dataset: RandomXDataset) -> Result<(), RandomXError> {
        if self.flags.contains(RandomXFlag::FLAG_FULL_MEM) {
            unsafe {
                randomx_vm_set_dataset(self.vm, dataset.inner.dataset_ptr);
            }
            self.linked_dataset = Some(dataset);
            Ok(())
        } else {
            Err(RandomXError::FlagConfigError(
                "Cannot reinit dataset without FLAG_FULL_MEM set".to_string(),
            ))
        }
    }

    /// Calculates a RandomX hash value and returns it, error on failure.
    ///
    /// `input` is a sequence of u8 to be hashed.
    pub fn calculate_hash(&self, input: &[u8]) -> Result<Vec<u8>, RandomXError> {
        if input.is_empty() {
            Err(RandomXError::ParameterError("input was empty".to_string()))
        } else {
            let size_input = input.len();
            let input_ptr = input.as_ptr() as *mut c_void;
            let arr = [0; RANDOMX_HASH_SIZE as usize];
            let output_ptr = arr.as_ptr() as *mut c_void;
            unsafe {
                randomx_calculate_hash(self.vm, input_ptr, size_input, output_ptr);
            }
            // if this failed, arr should still be empty
            if arr == [0; RANDOMX_HASH_SIZE as usize] {
                Err(RandomXError::Other("RandomX calculated hash was empty".to_string()))
            } else {
                let result = arr.to_vec();
                Ok(result)
            }
        }
    }

    /// Incremental hashing: initialize the VM state for a mining loop.
    ///
    /// Call this once at the start of a nonce range, then call `hash_next` for each
    /// subsequent nonce (including the first one), and finally `hash_last` to get the
    /// last hash. This avoids reinitializing the VM on every call and eliminates
    /// per-iteration allocations — ideal for mining workloads that hash millions of
    /// nonces against the same dataset.
    pub fn hash_first(&self, input: &[u8]) -> Result<(), RandomXError> {
        if input.is_empty() {
            return Err(RandomXError::ParameterError("input was empty".to_string()));
        }
        unsafe {
            randomx_calculate_hash_first(
                self.vm,
                input.as_ptr() as *const c_void,
                input.len(),
            );
        }
        Ok(())
    }

    /// Incremental hashing: hash the next nonce and write the previous result to `output`.
    ///
    /// Call this for each nonce in the range (including the first one after `hash_first`).
    /// The output buffer must be at least 32 bytes. Returns error if the VM state is invalid.
    pub fn hash_next(&self, input: &[u8], output: &mut [u8; RANDOMX_HASH_SIZE as usize]) -> Result<(), RandomXError> {
        if input.is_empty() {
            return Err(RandomXError::ParameterError("input was empty".to_string()));
        }
        unsafe {
            randomx_calculate_hash_next(
                self.vm,
                input.as_ptr() as *const c_void,
                input.len(),
                output.as_mut_ptr() as *mut c_void,
            );
        }
        if *output == [0; RANDOMX_HASH_SIZE as usize] {
            Err(RandomXError::Other("RandomX calculated hash was zero".to_string()))
        } else {
            Ok(())
        }
    }

    /// Incremental hashing: finalize and write the last hash to `output`.
    ///
    /// Call this after all nonces have been hashed via `hash_next`. The output buffer
    /// must be at least 32 bytes. Returns error if no hashes were computed.
    pub fn hash_last(&self, output: &mut [u8; RANDOMX_HASH_SIZE as usize]) -> Result<(), RandomXError> {
        unsafe {
            randomx_calculate_hash_last(self.vm, output.as_mut_ptr() as *mut c_void);
        }
        if *output == [0; RANDOMX_HASH_SIZE as usize] {
            Err(RandomXError::Other("RandomX calculated hash was zero".to_string()))
        } else {
            Ok(())
        }
    }

    /// Calculates hashes from a set of inputs.
    ///
    /// `input` is an array of a sequence of u8 to be hashed.
    #[allow(clippy::needless_range_loop)] // Range loop is not only for indexing `input`
    pub fn calculate_hash_set(&self, input: &[&[u8]]) -> Result<Vec<Vec<u8>>, RandomXError> {
        if input.is_empty() {
            // Empty set
            return Err(RandomXError::ParameterError("input was empty".to_string()));
        }

        let mut result = Vec::new();
        // For single input
        if input.len() == 1 {
            let hash = self.calculate_hash(input[0])?;
            result.push(hash);
            return Ok(result);
        }

        // For multiple inputs
        let mut output_ptr: *mut c_void = ptr::null_mut();
        let arr = [0; RANDOMX_HASH_SIZE as usize];

        // Not len() as last iteration assigns final hash
        let iterations = input.len() + 1;
        for i in 0..iterations {
            if i == iterations - 1 {
                // For last iteration
                unsafe {
                    randomx_calculate_hash_last(self.vm, output_ptr);
                }
            } else {
                if input[i].is_empty() {
                    // Stop calculations
                    if arr != [0; RANDOMX_HASH_SIZE as usize] {
                        // Complete what was started
                        unsafe {
                            randomx_calculate_hash_last(self.vm, output_ptr);
                        }
                    }
                    return Err(RandomXError::ParameterError("input was empty".to_string()));
                };
                let size_input = input[i].len();
                let input_ptr = input[i].as_ptr() as *mut c_void;
                output_ptr = arr.as_ptr() as *mut c_void;
                if i == 0 {
                    // For first iteration
                    unsafe {
                        randomx_calculate_hash_first(self.vm, input_ptr, size_input);
                    }
                } else {
                    unsafe {
                        // For every other iteration
                        randomx_calculate_hash_next(self.vm, input_ptr, size_input, output_ptr);
                    }
                }
            }

            if i != 0 {
                // First hash is only available in 2nd iteration
                if arr == [0; RANDOMX_HASH_SIZE as usize] {
                    return Err(RandomXError::Other("RandomX hash was zero".to_string()));
                }
                let output: Vec<u8> = arr.to_vec();
                result.push(output);
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::TryFrom, ptr, sync::{Arc, Mutex}};

    use crate::{
        bindings::randomx_get_dataset_memory,
        RandomXCache,
        RandomXCacheInner,
        RandomXDataset,
        RandomXDatasetInner,
        RandomXFlag,
        RandomXVM,
        RANDOMX_DATASET_ITEM_SIZE,
    };

    #[test]
    fn lib_alloc_cache() {
        let flags = RandomXFlag::default();
        let key = "Key";
        let cache = RandomXCache::new(flags, key.as_bytes()).expect("Failed to allocate cache");
        drop(cache);
    }

    #[test]
    fn lib_alloc_dataset() {
        let flags = RandomXFlag::default();
        let key = "Key";
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset = RandomXDataset::new(flags, cache.clone(), 0).expect("Failed to allocate dataset");
        drop(dataset);
        drop(cache);
    }

    #[test]
    fn lib_alloc_vm() {
        let flags = RandomXFlag::default();
        let key = "Key";
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let mut vm = RandomXVM::new(flags, Some(cache.clone()), None).expect("Failed to allocate VM");
        drop(vm);
        let dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
        vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).expect("Failed to allocate VM");
        drop(dataset);
        drop(cache);
        drop(vm);
    }

    #[test]
    fn lib_dataset_memory() {
        let flags = RandomXFlag::default();
        let key = "Key";
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
        let item_count = usize::try_from(RandomXDataset::count().unwrap()).unwrap();
        let memory = dataset.get_data().expect("Failed to get dataset memory");
        // `get_data` must return the *whole* dataset: one item is `RANDOMX_DATASET_ITEM_SIZE` bytes.
        assert_eq!(
            memory.len(),
            item_count * RANDOMX_DATASET_ITEM_SIZE,
            "get_data did not return the full dataset"
        );
        // Check the *last* item, which is the part of the range the length fix actually extended, rather than
        // scanning from the front (which would short-circuit on the very first byte and prove nothing).
        assert!(
            memory[memory.len() - RANDOMX_DATASET_ITEM_SIZE..]
                .iter()
                .any(|&b| b != 0),
            "The last dataset item was all zeroes"
        );
        drop(memory);
        drop(dataset);
        drop(cache);
    }

    #[test]
    fn lib_dataset_non_zero_start() {
        const START: u32 = 2;
        let flags = RandomXFlag::default();
        let key = "Key";
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let item_count = RandomXDataset::count().unwrap();
        let dataset = RandomXDataset::new(flags, cache.clone(), START).expect("Failed to allocate dataset");

        // Read the library's buffer directly rather than through `get_data`, so this test does not allocate a
        // second ~2 GB copy on top of the dataset itself.
        let memory = unsafe { randomx_get_dataset_memory(dataset.inner.dataset_ptr) };
        assert!(!memory.is_null());
        let len = usize::try_from(item_count).unwrap() * RANDOMX_DATASET_ITEM_SIZE;
        // SAFETY: `memory` is the non-null dataset buffer of `item_count` items, kept alive by `dataset` for the
        // duration of the borrow. Only `[START, item_count)` has been initialised; the leading `START` items are
        // undefined (see the docs on `new`), so this test deliberately does not read or assert on them.
        let data = unsafe { std::slice::from_raw_parts(memory.cast::<u8>(), len) };

        // The final item must be initialised, i.e. the range is not *under*-initialised. Note that this assertion
        // does not by itself catch a reintroduced overflow: the last item is written both when the correct
        // remaining count is passed and when the full item count is passed. What catches that regression is the
        // RandomX library's own `assert(startItem + itemCount <= DatasetItemCount)` in `randomx.cpp`, which is live
        // in debug builds (the `cmake` crate maps a debug Rust profile to `CMAKE_BUILD_TYPE=Debug`) and aborts the
        // test binary. CI runs the suite in debug, so a regression here fails the build.
        assert!(
            data[len - RANDOMX_DATASET_ITEM_SIZE..].iter().any(|&b| b != 0),
            "The last dataset item was not initialised"
        );

        // `start` must stay inside the dataset.
        assert!(RandomXDataset::new(flags, cache.clone(), item_count).is_err());

        drop(dataset);
        drop(cache);
    }

    #[test]
    fn lib_dataset_copy_from() {
        let flags = RandomXFlag::default();
        let key = "Key";
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        // Uninitialized buffers are enough — copy_from only moves raw bytes.
        let src = RandomXDataset::new_uninitialized(flags, cache.clone()).unwrap();
        let dst = RandomXDataset::new_uninitialized(flags, cache.clone()).unwrap();

        // memory_size must agree with the C-level item count * 64 bytes.
        let item_count = RandomXDataset::count().unwrap() as usize;
        assert_eq!(src.memory_size(), item_count * 64);
        assert!(src.memory_size() > 0);

        // Write a recognizable pattern into the source's raw buffer, copy it
        // across, and verify it transferred byte-exact.
        let size = src.memory_size();
        unsafe {
            for off in (0..size).step_by(64) {
                *src.raw_memory().add(off) = 0xAB;
            }
            *src.raw_memory().add(size - 1) = 0xCD;
        }
        dst.copy_from(&src).unwrap();
        unsafe {
            for off in (0..size).step_by(64) {
                assert_eq!(*dst.raw_memory().add(off), 0xAB);
            }
            assert_eq!(*dst.raw_memory().add(size - 1), 0xCD);
        }

        // Clones share one underlying buffer (Arc) — copy_from between a
        // dataset and its own clone must succeed as a no-op.
        let clone = dst.clone();
        dst.copy_from(&clone).unwrap();

        drop(src);
        drop(dst);
        drop(cache);
    }

    #[test]
    fn test_null_assignments() {
        let flags = RandomXFlag::get_recommended_flags();
        if let Ok(mut vm) = RandomXVM::new(flags, None, None) {
            let cache = RandomXCache {
                inner: Arc::new(RandomXCacheInner {
                    cache_ptr: ptr::null_mut(),
                }),
            };
            assert!(vm.reinit_cache(cache.clone()).is_err());
            let dataset = RandomXDataset {
                inner: Arc::new(RandomXDatasetInner {
                    dataset_ptr: ptr::null_mut(),
                    dataset_count: 0,
                    cache: Mutex::new(cache),
                }),
            };
            assert!(vm.reinit_dataset(dataset.clone()).is_err());
        }
    }

    #[test]
    fn lib_calculate_hash() {
        let flags = RandomXFlag::get_recommended_flags();
        let flags2 = flags | RandomXFlag::FLAG_FULL_MEM;
        let key = "Key";
        let input = "Input";
        let cache1 = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let mut vm1 = RandomXVM::new(flags, Some(cache1.clone()), None).unwrap();
        let hash1 = vm1.calculate_hash(input.as_bytes()).expect("no data");
        let vec = vec![0u8; hash1.len()];
        assert_ne!(hash1, vec);
        let reinit_cache = vm1.reinit_cache(cache1.clone());
        assert!(reinit_cache.is_ok());
        let hash2 = vm1.calculate_hash(input.as_bytes()).expect("no data");
        assert_ne!(hash2, vec);
        assert_eq!(hash1, hash2);

        let cache2 = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let vm2 = RandomXVM::new(flags, Some(cache2.clone()), None).unwrap();
        let hash3 = vm2.calculate_hash(input.as_bytes()).expect("no data");
        assert_eq!(hash2, hash3);

        let cache3 = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset3 = RandomXDataset::new(flags, cache3.clone(), 0).unwrap();
        let mut vm3 = RandomXVM::new(flags2, None, Some(dataset3.clone())).unwrap();
        let hash4 = vm3.calculate_hash(input.as_bytes()).expect("no data");
        assert_ne!(hash3, vec);
        let reinit_dataset = vm3.reinit_dataset(dataset3.clone());
        assert!(reinit_dataset.is_ok());
        let hash5 = vm3.calculate_hash(input.as_bytes()).expect("no data");
        assert_ne!(hash4, vec);
        assert_eq!(hash4, hash5);

        let cache4 = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset4 = RandomXDataset::new(flags, cache4.clone(), 0).unwrap();
        let vm4 = RandomXVM::new(flags2, Some(cache4), Some(dataset4.clone())).unwrap();
        let hash6 = vm3.calculate_hash(input.as_bytes()).expect("no data");
        assert_eq!(hash5, hash6);

        drop(dataset3);
        drop(dataset4);
        drop(cache1);
        drop(cache2);
        drop(cache3);
        drop(vm1);
        drop(vm2);
        drop(vm3);
        drop(vm4);
    }

    #[test]
    fn lib_calculate_hash_set() {
        let flags = RandomXFlag::default();
        let key = "Key";
        let inputs = vec!["Input".as_bytes(), "Input 2".as_bytes(), "Inputs 3".as_bytes()];
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap();
        let hashes = vm.calculate_hash_set(inputs.as_slice()).expect("no data");
        assert_eq!(inputs.len(), hashes.len());
        let mut prev_hash = Vec::new();
        for (i, hash) in hashes.into_iter().enumerate() {
            let vec = vec![0u8; hash.len()];
            assert_ne!(hash, vec);
            assert_ne!(hash, prev_hash);
            let compare = vm.calculate_hash(inputs[i]).unwrap(); // sanity check
            assert_eq!(hash, compare);
            prev_hash = hash;
        }
        drop(cache);
        drop(vm);
    }

    #[test]
    fn lib_calculate_hash_is_consistent() {
        let flags = RandomXFlag::get_recommended_flags();
        let key = "Key";
        let input = "Input";
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
        let vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
        let hash = vm.calculate_hash(input.as_bytes()).expect("no data");
        assert_eq!(hash, [
            114, 81, 192, 5, 165, 242, 107, 100, 184, 77, 37, 129, 52, 203, 217, 227, 65, 83, 215, 213, 59, 71, 32,
            172, 253, 155, 204, 111, 183, 213, 157, 155
        ]);
        drop(vm);
        drop(dataset);
        drop(cache);

        let cache1 = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset1 = RandomXDataset::new(flags, cache1.clone(), 0).unwrap();
        let vm1 = RandomXVM::new(flags, Some(cache1.clone()), Some(dataset1.clone())).unwrap();
        let hash1 = vm1.calculate_hash(input.as_bytes()).expect("no data");
        assert_eq!(hash1, [
            114, 81, 192, 5, 165, 242, 107, 100, 184, 77, 37, 129, 52, 203, 217, 227, 65, 83, 215, 213, 59, 71, 32,
            172, 253, 155, 204, 111, 183, 213, 157, 155
        ]);
        drop(vm1);
        drop(dataset1);
        drop(cache1);
    }

    #[test]
    fn lib_check_cache_and_dataset_lifetimes() {
        let flags = RandomXFlag::get_recommended_flags();
        let key = "Key";
        let input = "Input";
        let cache = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
        let vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
        drop(dataset);
        drop(cache);
        let hash = vm.calculate_hash(input.as_bytes()).expect("no data");
        assert_eq!(hash, [
            114, 81, 192, 5, 165, 242, 107, 100, 184, 77, 37, 129, 52, 203, 217, 227, 65, 83, 215, 213, 59, 71, 32,
            172, 253, 155, 204, 111, 183, 213, 157, 155
        ]);
        drop(vm);

        let cache1 = RandomXCache::new(flags, key.as_bytes()).unwrap();
        let dataset1 = RandomXDataset::new(flags, cache1.clone(), 0).unwrap();
        let vm1 = RandomXVM::new(flags, Some(cache1.clone()), Some(dataset1.clone())).unwrap();
        drop(dataset1);
        drop(cache1);
        let hash1 = vm1.calculate_hash(input.as_bytes()).expect("no data");
        assert_eq!(hash1, [
            114, 81, 192, 5, 165, 242, 107, 100, 184, 77, 37, 129, 52, 203, 217, 227, 65, 83, 215, 213, 59, 71, 32,
            172, 253, 155, 204, 111, 183, 213, 157, 155
        ]);
        drop(vm1);
    }

    #[test]
    fn randomx_hash_fast_vs_light() {
        let input = b"input";
        let key = b"key";

        let flags = RandomXFlag::get_recommended_flags() | RandomXFlag::FLAG_FULL_MEM;
        let cache = RandomXCache::new(flags, key).unwrap();
        let dataset = RandomXDataset::new(flags, cache, 0).unwrap();
        let fast_vm = RandomXVM::new(flags, None, Some(dataset)).unwrap();

        let flags = RandomXFlag::get_recommended_flags();
        let cache = RandomXCache::new(flags, key).unwrap();
        let light_vm = RandomXVM::new(flags, Some(cache), None).unwrap();

        let fast = fast_vm.calculate_hash(input).unwrap();
        let light = light_vm.calculate_hash(input).unwrap();
        assert_eq!(fast, light);
    }

    #[test]
    fn test_vectors_fast_mode() {
        // test vectors from https://github.com/tevador/RandomX/blob/040f4500a6e79d54d84a668013a94507045e786f/src/tests/tests.cpp#L963-L979
        let key = b"test key 000";
        let vectors = [
            (
                b"This is a test".as_slice(),
                "639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f",
            ),
            (
                b"Lorem ipsum dolor sit amet".as_slice(),
                "300a0adb47603dedb42228ccb2b211104f4da45af709cd7547cd049e9489c969",
            ),
            (
                b"sed do eiusmod tempor incididunt ut labore et dolore magna aliqua".as_slice(),
                "c36d4ed4191e617309867ed66a443be4075014e2b061bcdaf9ce7b721d2b77a8",
            ),
        ];

        let flags = RandomXFlag::get_recommended_flags() | RandomXFlag::FLAG_FULL_MEM;
        let cache = RandomXCache::new(flags, key).unwrap();
        let dataset = RandomXDataset::new(flags, cache, 0).unwrap();
        let vm = RandomXVM::new(flags, None, Some(dataset)).unwrap();

        for (input, expected) in vectors {
            let hash = vm.calculate_hash(input).unwrap();
            assert_eq!(hex::decode(expected).unwrap(), hash);
        }
    }

    #[test]
    fn test_vectors_fast_mode_sliced_and_reinit() {
        // Sliced init (new_uninitialized + init_range, as a miner would call it
        // from multiple threads) must produce the same dataset as a single pass,
        // and reinit_with_cache must switch the dataset to a new seed in place.
        // Test vectors from https://github.com/tevador/RandomX/blob/040f4500a6e79d54d84a668013a94507045e786f/src/tests/tests.cpp#L963-L979
        let vectors: &[(&[u8], &str)] = &[
            (
                b"This is a test".as_slice(),
                "639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f",
            ),
            (
                b"Lorem ipsum dolor sit amet".as_slice(),
                "300a0adb47603dedb42228ccb2b211104f4da45af709cd7547cd049e9489c969",
            ),
            (
                b"sed do eiusmod tempor incididunt ut labore et dolore magna aliqua".as_slice(),
                "c36d4ed4191e617309867ed66a443be4075014e2b061bcdaf9ce7b721d2b77a8",
            ),
        ];
        // The only upstream known-answer vector for key "test key 001"
        // (listed for both fast and light mode in the RandomX test suite).
        let reinit_vectors: &[(&[u8], &str)] = &[
            (
                b"sed do eiusmod tempor incididunt ut labore et dolore magna aliqua".as_slice(),
                "e9ff4503201c0c2cca26d285c93ae883f9b1d30c9eb240b820756f2d5a7905fc",
            ),
        ];

        let flags = RandomXFlag::get_recommended_flags() | RandomXFlag::FLAG_FULL_MEM;

        // Build the dataset in two disjoint slices (split point at 1/4).
        let cache = RandomXCache::new(flags, b"test key 000").unwrap();
        let dataset = RandomXDataset::new_uninitialized(flags, cache).unwrap();
        let item_count = RandomXDataset::count().unwrap();
        let split = item_count / 4;
        dataset.init_range(0, split).unwrap();
        dataset.init_range(split, item_count - split).unwrap();

        let vm = RandomXVM::new(flags, None, Some(dataset.clone())).unwrap();
        for (input, expected) in vectors {
            let hash = vm.calculate_hash(input).unwrap();
            assert_eq!(hex::decode(expected).unwrap(), hash);
        }

        // Reinit the SAME dataset object (and its buffer) with a new cache/seed.
        let cache2 = RandomXCache::new(flags, b"test key 001").unwrap();
        dataset.reinit_with_cache(&cache2).unwrap();
        dataset.init_range(0, item_count).unwrap();

        for (input, expected) in reinit_vectors {
            let hash = vm.calculate_hash(input).unwrap();
            assert_eq!(hex::decode(expected).unwrap(), hash);
        }
    }

    #[test]
    fn test_vectors_light_mode() {
        // test vectors from https://github.com/tevador/RandomX/blob/040f4500a6e79d54d84a668013a94507045e786f/src/tests/tests.cpp#L963-L985
        let vectors = [
            (
                b"test key 000",
                b"This is a test".as_slice(),
                "639183aae1bf4c9a35884cb46b09cad9175f04efd7684e7262a0ac1c2f0b4e3f",
            ),
            (
                b"test key 000",
                b"Lorem ipsum dolor sit amet".as_slice(),
                "300a0adb47603dedb42228ccb2b211104f4da45af709cd7547cd049e9489c969",
            ),
            (
                b"test key 000",
                b"sed do eiusmod tempor incididunt ut labore et dolore magna aliqua".as_slice(),
                "c36d4ed4191e617309867ed66a443be4075014e2b061bcdaf9ce7b721d2b77a8",
            ),
            (
                b"test key 001",
                b"sed do eiusmod tempor incididunt ut labore et dolore magna aliqua".as_slice(),
                "e9ff4503201c0c2cca26d285c93ae883f9b1d30c9eb240b820756f2d5a7905fc",
            ),
        ];

        let flags = RandomXFlag::get_recommended_flags();
        for (key, input, expected) in vectors {
            let cache = RandomXCache::new(flags, key).unwrap();
            let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
            let hash = vm.calculate_hash(input).unwrap();
            assert_eq!(hex::decode(expected).unwrap(), hash);
        }
    }
}
