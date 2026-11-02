//! Thin Rust wrapper around the Swift Foundation Models bridge.
//!
//! The bridge is compiled by `build.rs` on macOS and exposes three C
//! functions. On macOS 27+ with Apple Intelligence enabled it answers through
//! the OS model; on older systems it reports itself unavailable so Prompt
//! falls back to Qwen.

#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::{CStr, CString};
    use std::os::raw::{c_char, c_int};

    unsafe extern "C" {
        fn prompt_fm_available() -> c_int;
        fn prompt_fm_generate(prompt: *const c_char) -> *mut c_char;
        fn prompt_fm_free(pointer: *mut c_char);
    }

    pub fn available() -> bool {
        // Safety: the bridge is linked into the binary on macOS.
        unsafe { prompt_fm_available() != 0 }
    }

    pub fn generate(prompt: &str) -> Result<String, String> {
        let prompt = CString::new(prompt).map_err(|_| "prompt contains a NUL byte".to_string())?;
        // Safety: `prompt` is a valid NUL-terminated string. The bridge returns
        // either NULL or a `strdup` allocation that we free below.
        let pointer = unsafe { prompt_fm_generate(prompt.as_ptr()) };
        if pointer.is_null() {
            return Err("Apple Foundation Models is unavailable or returned no result".into());
        }
        let output = unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned();
        unsafe { prompt_fm_free(pointer) };
        Ok(output)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn generate(_: &str) -> Result<String, String> {
        Err("Apple Foundation Models is only available on macOS".into())
    }
}

#[cfg(target_os = "macos")]
pub use imp::available;
pub use imp::generate;
