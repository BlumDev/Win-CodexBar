//! Single instance detection using Windows named mutex
//!
//! Prevents multiple instances of the menubar app from running simultaneously.

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE};
#[cfg(windows)]
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
#[cfg(windows)]
use windows::core::PCWSTR;

/// Guard that holds the single instance mutex
/// When dropped, the mutex is released
pub struct SingleInstanceGuard {
    #[cfg(windows)]
    handle: HANDLE,
    #[cfg(not(windows))]
    _marker: std::marker::PhantomData<()>,
}

impl SingleInstanceGuard {
    /// Mutex name for CodexBar — uses Local namespace to restrict to current session,
    /// preventing other users/sessions from blocking startup.
    const MUTEX_NAME: &'static str = "Local\\CodexBar_SingleInstance_Mutex";

    /// Try to acquire the single instance lock
    /// Returns Some(guard) if this is the first instance, None if another instance is running
    #[cfg(windows)]
    pub fn try_acquire() -> Option<Self> {
        use windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
        use windows::Win32::Foundation::GetLastError;

        // Convert mutex name to wide string
        let wide_name: Vec<u16> = Self::MUTEX_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let handle = CreateMutexW(
                None, // default security attributes
                true, // initially owned
                PCWSTR(wide_name.as_ptr()),
            );

            match handle {
                Ok(h) => {
                    // Check if mutex already existed
                    let last_error = GetLastError();
                    if last_error == ERROR_ALREADY_EXISTS {
                        // Another instance is running, close our handle
                        let _ = CloseHandle(h);
                        None
                    } else {
                        // We're the first instance
                        Some(Self { handle: h })
                    }
                }
                Err(_) => {
                    // Failed to create mutex, allow running anyway
                    tracing::warn!("Failed to create single instance mutex");
                    None
                }
            }
        }
    }

    /// Non-Windows stub - always succeeds
    #[cfg(not(windows))]
    pub fn try_acquire() -> Option<Self> {
        Some(Self {
            _marker: std::marker::PhantomData,
        })
    }
}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        // No-op on non-Windows
    }
}

/// Restore and center an already-running CodexBar window.
#[cfg(windows)]
pub fn activate_existing_instance() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetWindowRect, IsIconic, MoveWindow, SPI_GETWORKAREA, SW_RESTORE, SW_SHOW,
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetForegroundWindow, ShowWindow,
        SystemParametersInfoW,
    };
    use windows::core::w;

    unsafe {
        let Ok(hwnd) = FindWindowW(None, w!("CodexBar")) else {
            return false;
        };
        if hwnd.is_invalid() {
            return false;
        }

        let mut window_rect = RECT::default();
        let mut work_area = RECT::default();
        if GetWindowRect(hwnd, &mut window_rect).is_err()
            || SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut work_area as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .is_err()
        {
            return false;
        }

        let width = (window_rect.right - window_rect.left).max(680);
        let height = (window_rect.bottom - window_rect.top).max(820);
        let x = work_area.left + ((work_area.right - work_area.left - width).max(0) / 2);
        let y = work_area.top + ((work_area.bottom - work_area.top - height).max(0) / 2);

        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = MoveWindow(hwnd, x, y, width, height, true);
        let _ = SetForegroundWindow(hwnd);
        true
    }
}

#[cfg(not(windows))]
pub fn activate_existing_instance() -> bool {
    false
}
