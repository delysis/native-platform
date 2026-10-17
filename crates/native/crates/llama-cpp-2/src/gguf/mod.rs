//! Safe wrapper around `gguf_context` for reading GGUF file metadata.
//!
//! Provides metadata-only access to GGUF files without loading tensor data.
//! Useful for inspecting model architecture parameters before loading a model.

use std::ffi::{CStr, CString};
use std::path::Path;
use std::ptr::NonNull;
use std::slice;

const MAX_TYPED_ARRAY_ITEMS: usize = 1_048_576;

/// A safe wrapper around `gguf_context`.
///
/// Opens a GGUF file and parses only the metadata header; tensor weights are
/// never loaded into memory (`no_alloc = true`).
#[derive(Debug)]
pub struct GgufContext {
    ctx: NonNull<llama_cpp_sys_2::gguf_context>,
}

impl GgufContext {
    /// Open a GGUF file and parse its metadata header.
    ///
    /// Returns `None` if the path contains a null byte, the file does not
    /// exist, or the file is not a valid GGUF file.
    pub fn from_file(path: &Path) -> Option<Self> {
        let c_path = CString::new(path.to_str()?).ok()?;
        let params = llama_cpp_sys_2::gguf_init_params {
            no_alloc: true,
            ctx: std::ptr::null_mut(),
        };
        let ptr = unsafe { llama_cpp_sys_2::gguf_init_from_file(c_path.as_ptr(), params) };
        Some(Self {
            ctx: NonNull::new(ptr)?,
        })
    }

    /// Total number of key-value pairs in the metadata.
    pub fn n_kv(&self) -> i64 {
        unsafe { llama_cpp_sys_2::gguf_get_n_kv(self.ctx.as_ptr()) }
    }

    /// Find the index of a key by name. Returns `-1` if not found.
    pub fn find_key(&self, key: &str) -> i64 {
        let Ok(c_key) = CString::new(key) else {
            return -1;
        };
        unsafe { llama_cpp_sys_2::gguf_find_key(self.ctx.as_ptr(), c_key.as_ptr()) }
    }

    /// Return the key name at the given index, or `None` if out of range.
    pub fn key_at(&self, idx: i64) -> Option<&str> {
        let ptr = unsafe { llama_cpp_sys_2::gguf_get_key(self.ctx.as_ptr(), idx) };
        if ptr.is_null() {
            return None;
        }
        unsafe { CStr::from_ptr(ptr).to_str().ok() }
    }

    /// Return the value type of the KV pair at `idx`.
    pub fn kv_type(&self, idx: i64) -> llama_cpp_sys_2::gguf_type {
        unsafe { llama_cpp_sys_2::gguf_get_kv_type(self.ctx.as_ptr(), idx) }
    }

    /// Return the element type of an array-valued metadata entry.
    pub fn array_type(&self, idx: i64) -> Option<llama_cpp_sys_2::gguf_type> {
        self.valid_array_len(idx)?;
        Some(unsafe { llama_cpp_sys_2::gguf_get_arr_type(self.ctx.as_ptr(), idx) })
    }

    /// Copy a bounded `int32` metadata array into Rust-owned memory.
    pub fn val_array_i32(&self, idx: i64) -> Option<Vec<i32>> {
        let len = self.valid_array_len(idx)?;
        if unsafe { llama_cpp_sys_2::gguf_get_arr_type(self.ctx.as_ptr(), idx) }
            != llama_cpp_sys_2::GGUF_TYPE_INT32
        {
            return None;
        }
        let data = unsafe { llama_cpp_sys_2::gguf_get_arr_data(self.ctx.as_ptr(), idx) };
        if len == 0 {
            return Some(Vec::new());
        }
        if data.is_null() {
            return None;
        }
        // The checked GGUF element type guarantees aligned i32 storage, and
        // the GGUF context owns this array for the duration of the copy.
        Some(unsafe { slice::from_raw_parts(data.cast::<i32>(), len) }.to_vec())
    }

    /// Copy a bounded `bool` metadata array into Rust-owned memory.
    pub fn val_array_bool(&self, idx: i64) -> Option<Vec<bool>> {
        let len = self.valid_array_len(idx)?;
        if unsafe { llama_cpp_sys_2::gguf_get_arr_type(self.ctx.as_ptr(), idx) }
            != llama_cpp_sys_2::GGUF_TYPE_BOOL
        {
            return None;
        }
        let data = unsafe { llama_cpp_sys_2::gguf_get_arr_data(self.ctx.as_ptr(), idx) };
        if len == 0 {
            return Some(Vec::new());
        }
        if data.is_null() {
            return None;
        }
        // Read bool storage as bytes first so malformed metadata cannot create
        // an invalid Rust `bool` value through the FFI boundary.
        let values = unsafe { slice::from_raw_parts(data.cast::<u8>(), len) };
        values
            .iter()
            .map(|value| match value {
                0 => Some(false),
                1 => Some(true),
                _ => None,
            })
            .collect()
    }

    /// Read a `uint32` value. Panics (inside llama.cpp) if the stored type is
    /// not `GGUF_TYPE_UINT32` — check `kv_type` first if unsure.
    pub fn val_u32(&self, idx: i64) -> u32 {
        unsafe { llama_cpp_sys_2::gguf_get_val_u32(self.ctx.as_ptr(), idx) }
    }

    /// Read an `int32` value.
    pub fn val_i32(&self, idx: i64) -> i32 {
        unsafe { llama_cpp_sys_2::gguf_get_val_i32(self.ctx.as_ptr(), idx) }
    }

    /// Read a `uint64` value.
    pub fn val_u64(&self, idx: i64) -> u64 {
        unsafe { llama_cpp_sys_2::gguf_get_val_u64(self.ctx.as_ptr(), idx) }
    }

    /// Read a string value. Returns `None` if the pointer is null or not
    /// valid UTF-8.
    pub fn val_str(&self, idx: i64) -> Option<&str> {
        let ptr = unsafe { llama_cpp_sys_2::gguf_get_val_str(self.ctx.as_ptr(), idx) };
        if ptr.is_null() {
            return None;
        }
        unsafe { CStr::from_ptr(ptr).to_str().ok() }
    }

    /// Total number of tensors described in the file.
    pub fn n_tensors(&self) -> i64 {
        unsafe { llama_cpp_sys_2::gguf_get_n_tensors(self.ctx.as_ptr()) }
    }

    fn valid_array_len(&self, idx: i64) -> Option<usize> {
        if idx < 0 || idx >= self.n_kv() || self.kv_type(idx) != llama_cpp_sys_2::GGUF_TYPE_ARRAY {
            return None;
        }
        let len = unsafe { llama_cpp_sys_2::gguf_get_arr_n(self.ctx.as_ptr(), idx) };
        (len <= MAX_TYPED_ARRAY_ITEMS).then_some(len)
    }
}

impl Drop for GgufContext {
    fn drop(&mut self) {
        unsafe { llama_cpp_sys_2::gguf_free(self.ctx.as_ptr()) }
    }
}

#[cfg(test)]
mod tests;
