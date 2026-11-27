//! Helpers every FFI module shares: the one `host_log` and the C-string readers.
//!
//! Every host table the player installs carries the same `log` callback, so each
//! `acnh_*_set_host` hands it to [`set_log`] and the whole crate logs through [`host_log`].

use std::ffi::{c_char, CStr, CString};
use std::sync::Mutex;

/// The player's log callback (`const char*` in, one line out).
pub(crate) type LogFn = extern "C" fn(*const c_char);

static LOG: Mutex<Option<LogFn>> = Mutex::new(None);

/// Remembers the log callback from a host table; a table without one leaves the last one in place.
pub(crate) fn set_log(f: Option<LogFn>) {
    if f.is_some() {
        *LOG.lock().unwrap_or_else(|e| e.into_inner()) = f;
    }
}

/// Sends one line to the player's log. A NUL byte inside `msg` becomes a space; with no callback
/// installed yet the line is dropped.
pub(crate) fn host_log(msg: &str) {
    let f = *LOG.lock().unwrap_or_else(|e| e.into_inner());
    let Some(f) = f else {
        return;
    };
    if let Ok(c) = CString::new(msg.replace('\0', " ")) {
        f(c.as_ptr());
    }
}

/// The UTF-8 text at `ptr`, or `None` for null or invalid UTF-8.
pub(crate) fn cstr_opt<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `ptr` is a valid NUL-terminated C string when non-null.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Like [`cstr_opt`], but null and invalid UTF-8 both read as the empty string.
pub(crate) fn cstr_or_empty<'a>(ptr: *const c_char) -> &'a str {
    cstr_opt(ptr).unwrap_or("")
}

/// The text at `ptr` with invalid UTF-8 replaced; null reads as the empty string.
pub(crate) fn cstr_lossy(ptr: *const c_char) -> String {
    cstr_opt_lossy(ptr).unwrap_or_default()
}

/// The text at `ptr` with invalid UTF-8 replaced, or `None` for null.
pub(crate) fn cstr_opt_lossy(ptr: *const c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `ptr` is a valid NUL-terminated C string when non-null.
    Some(
        unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    static CALLS: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn sink(msg: *const c_char) {
        CALLS.fetch_add(1, Ordering::SeqCst);
        SEEN.lock().unwrap().push(cstr_lossy(msg));
    }

    #[test]
    fn host_log_reaches_the_installed_callback_and_blanks_nul_bytes() {
        set_log(Some(sink));
        set_log(None); // a table without a callback keeps the one already installed
        host_log("one\0two");
        assert!(SEEN.lock().unwrap().iter().any(|l| l == "one two"));
        assert!(CALLS.load(Ordering::SeqCst) >= 1);
    }

    #[test]
    fn the_readers_agree_on_null_and_bad_utf8() {
        let null = std::ptr::null();
        assert_eq!(cstr_opt(null), None);
        assert_eq!(cstr_or_empty(null), "");
        assert_eq!(cstr_lossy(null), "");
        assert_eq!(cstr_opt_lossy(null), None);
        let good = c"hello";
        assert_eq!(cstr_opt(good.as_ptr()), Some("hello"));
        assert_eq!(cstr_or_empty(good.as_ptr()), "hello");
        let bad = c"a\xffb";
        assert_eq!(cstr_opt(bad.as_ptr()), None);
        assert_eq!(cstr_or_empty(bad.as_ptr()), "");
        assert_eq!(cstr_lossy(bad.as_ptr()), "a\u{fffd}b");
        assert_eq!(cstr_opt_lossy(good.as_ptr()).as_deref(), Some("hello"));
    }
}
