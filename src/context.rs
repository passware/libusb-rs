mod raw_context_wrapper;
use self::raw_context_wrapper::RawContextWrapper;

use std::{mem::MaybeUninit, sync::Arc};

use libc::{c_char, c_int};
use libusb::*;

use device_handle::{self, DeviceHandle};
use device_list::{self, DeviceList};
use error;
use std::{ffi::CStr, sync::Mutex};

/// A `libusb` context.
pub struct Context {
    pub(crate) context: Arc<RawContextWrapper>,
}

impl Clone for Context {
    fn clone(&self) -> Self {
        Self {
            context: self.context.clone(),
        }
    }
}

unsafe impl Sync for Context {}
unsafe impl Send for Context {}

type LogCallback = Box<dyn Fn(LogLevel, String)>;

struct LogCallbackMap {
    map: std::collections::HashMap<*mut libusb_context, LogCallback>,
}

unsafe impl Sync for LogCallbackMap {}
unsafe impl Send for LogCallbackMap {}

impl LogCallbackMap {
    pub fn new() -> Self {
        Self {
            map: std::collections::HashMap::new(),
        }
    }
}

lazy_static::lazy_static! {
    static ref LOG_CALLBACK_MAP: Mutex<LogCallbackMap> = Mutex::new(LogCallbackMap::new());
    static ref DEFAULT_CONTEXT_INITIALIZED_FLAG: Mutex<bool> = Mutex::new(false);
}

extern "C" fn static_log_callback(context: *mut libusb_context, level: c_int, text: *const c_char) {
    if let Ok(locked_table) = LOG_CALLBACK_MAP.lock() {
        if let Some(logger) = locked_table.map.get(&context) {
            let c_str: &CStr = unsafe { CStr::from_ptr(text) };
            let str_slice: &str = c_str.to_str().unwrap_or("");
            let log_message = str_slice.to_owned();

            logger(LogLevel::from_c_int(level), log_message);
        }
    }
}

impl Context {
    /// Opens a new `libusb` context.
    pub fn new() -> ::Result<Self> {
        let mut context = unsafe { MaybeUninit::uninit().assume_init() };
        try_unsafe!(libusb_init(&mut context));
        Ok(Self {
            context: Arc::new(RawContextWrapper { context }),
        })
    }

    pub fn init_default_context() -> ::Result<()> {
        if let Ok(mut flag) = DEFAULT_CONTEXT_INITIALIZED_FLAG.lock() {
            if !*flag {
                try_unsafe!(libusb_init(std::ptr::null_mut()));
                *flag = true;
            }
        }
        Ok(())
    }

    pub fn release_default_context() {
        if let Ok(mut flag) = DEFAULT_CONTEXT_INITIALIZED_FLAG.lock() {
            if *flag {
                unsafe { libusb_exit(std::ptr::null_mut()) };
                *flag = false;
            }
        }
    }

    /// Sets the log level of a `libusb` context.
    pub fn set_log_level(&mut self, level: LogLevel) {
        unsafe {
            libusb_set_option(**self.context, LIBUSB_OPTION_LOG_LEVEL, level.as_c_int());
        }
    }

    /// Sets the log level for the default context.
    pub fn set_default_context_log_level(level: LogLevel) {
        unsafe {
            libusb_set_option(
                std::ptr::null_mut(),
                LIBUSB_OPTION_LOG_LEVEL,
                level.as_c_int(),
            );
        }
    }

    pub fn set_log_callback(&mut self, log_callback: LogCallback, mode: LogCallbackMode) {
        if let Ok(mut locked_table) = LOG_CALLBACK_MAP.lock() {
            match mode {
                LogCallbackMode::Global => {
                    locked_table.map.insert(std::ptr::null_mut(), log_callback)
                }
                LogCallbackMode::Context => locked_table.map.insert(**self.context, log_callback),
            };
        }

        unsafe {
            libusb_set_log_cb(**self.context, static_log_callback, mode.as_c_int());
        }
    }

    pub fn has_capability(&self) -> bool {
        unsafe { libusb_has_capability(LIBUSB_CAP_HAS_CAPABILITY) != 0 }
    }

    /// Tests whether the running `libusb` library supports hotplug.
    pub fn has_hotplug(&self) -> bool {
        unsafe { libusb_has_capability(LIBUSB_CAP_HAS_HOTPLUG) != 0 }
    }

    /// Tests whether the running `libusb` library has HID access.
    pub fn has_hid_access(&self) -> bool {
        unsafe { libusb_has_capability(LIBUSB_CAP_HAS_HID_ACCESS) != 0 }
    }

    /// Tests whether the running `libusb` library supports detaching the kernel driver.
    pub fn supports_detach_kernel_driver(&self) -> bool {
        unsafe { libusb_has_capability(LIBUSB_CAP_SUPPORTS_DETACH_KERNEL_DRIVER) != 0 }
    }

    /// Returns a list of the current USB devices. The context must outlive the device list.
    pub fn devices(&self) -> ::Result<DeviceList> {
        let mut list: *const *mut libusb_device = unsafe { MaybeUninit::uninit().assume_init() };

        let n = unsafe { libusb_get_device_list(**self.context, &mut list) };

        if n < 0 {
            Err(error::from_libusb(n as c_int))
        } else {
            Ok(unsafe { device_list::from_libusb(self.clone(), list, n as usize) })
        }
    }

    /// Convenience function to open a device by its vendor ID and product ID.
    ///
    /// This function is provided as a convenience for building prototypes without having to
    /// iterate a [`DeviceList`](struct.DeviceList.html). It is not meant for production
    /// applications.
    ///
    /// Returns a device handle for the first device found matching `vendor_id` and `product_id`.
    /// On error, or if the device could not be found, it returns `None`.
    pub fn open_device_with_vid_pid(
        &self,
        vendor_id: u16,
        product_id: u16,
    ) -> Option<DeviceHandle> {
        let handle =
            unsafe { libusb_open_device_with_vid_pid(**self.context, vendor_id, product_id) };

        if handle.is_null() {
            None
        } else {
            Some(unsafe { device_handle::from_libusb(self.clone(), handle) })
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        if let Ok(mut locked_table) = LOG_CALLBACK_MAP.lock() {
            locked_table.map.remove(&**self.context);
        }
    }
}

/// Library logging levels.
#[derive(Debug)]
pub enum LogLevel {
    /// No messages are printed by `libusb` (default).
    None,

    /// Error messages printed to `stderr`.
    Error,

    /// Warning and error messages are printed to `stderr`.
    Warning,

    /// Informational messages are printed to `stdout`. Warnings and error messages are printed to
    /// `stderr`.
    Info,

    /// Debug and informational messages are printed to `stdout`. Warnings and error messages are
    /// printed to `stderr`.
    Debug,
}

impl LogLevel {
    fn as_c_int(&self) -> c_int {
        match *self {
            LogLevel::None => LIBUSB_LOG_LEVEL_NONE,
            LogLevel::Error => LIBUSB_LOG_LEVEL_ERROR,
            LogLevel::Warning => LIBUSB_LOG_LEVEL_WARNING,
            LogLevel::Info => LIBUSB_LOG_LEVEL_INFO,
            LogLevel::Debug => LIBUSB_LOG_LEVEL_DEBUG,
        }
    }

    fn from_c_int(raw: c_int) -> LogLevel {
        match raw {
            LIBUSB_LOG_LEVEL_ERROR => LogLevel::Error,
            LIBUSB_LOG_LEVEL_WARNING => LogLevel::Warning,
            LIBUSB_LOG_LEVEL_INFO => LogLevel::Info,
            LIBUSB_LOG_LEVEL_DEBUG => LogLevel::Debug,
            _ => LogLevel::None,
        }
    }
}

pub enum LogCallbackMode {
    /// Callback function handling all log messages.
    Global,

    /// Callback function handling context related log messages.
    Context,
}

impl LogCallbackMode {
    fn as_c_int(&self) -> c_int {
        match *self {
            LogCallbackMode::Global => LIBUSB_LOG_CB_GLOBAL,
            LogCallbackMode::Context => LIBUSB_LOG_CB_CONTEXT,
        }
    }
}
