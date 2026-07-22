use std::ffi::{c_void, CString};
use std::os::raw::c_char;

#[allow(dead_code)]
pub mod log_level {
    pub const TRACE: i32 = 0;
    pub const DEBUG: i32 = 1;
    pub const INFO: i32 = 2;
    pub const WARN: i32 = 3;
    pub const ERROR: i32 = 4;
}

#[derive(Clone, Copy)]
pub struct LogSink {
    pub cb: Option<extern "C" fn(level: i32, msg: *const c_char, user: *mut c_void)>,
    pub user: *mut c_void,
}

impl LogSink {
    pub fn none() -> Self {
        LogSink {
            cb: None,
            user: std::ptr::null_mut(),
        }
    }

    pub fn log(&self, level: i32, msg: &str) {
        if let Some(cb) = self.cb {
            if let Ok(c) = CString::new(msg) {
                cb(level, c.as_ptr(), self.user);
            }
        }
    }
}
