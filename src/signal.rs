//! Ctrl+C を受け取る。1 回目はフラグを立てて後始末させ、2 回目で即終了する。

use std::sync::atomic::{AtomicU32, Ordering};

static COUNT: AtomicU32 = AtomicU32::new(0);

pub fn interrupted() -> bool {
    COUNT.load(Ordering::SeqCst) > 0
}

#[cfg(windows)]
mod imp {
    use super::COUNT;
    use std::sync::Once;
    use std::sync::atomic::Ordering;

    const CTRL_C_EVENT: u32 = 0;
    const CTRL_BREAK_EVENT: u32 = 1;

    // BOOL WINAPI SetConsoleCtrlHandler(PHANDLER_ROUTINE HandlerRoutine, BOOL Add);
    // BOOL WINAPI HandlerRoutine(DWORD dwCtrlType);
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }

    unsafe extern "system" fn handler(ctrl: u32) -> i32 {
        if ctrl != CTRL_C_EVENT && ctrl != CTRL_BREAK_EVENT {
            return 0;
        }
        if COUNT.fetch_add(1, Ordering::SeqCst) >= 1 {
            std::process::exit(130);
        }
        1
    }

    pub fn install() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            // SAFETY: handler は上の署名どおりの関数で、プロセスが終わるまで有効
            unsafe {
                SetConsoleCtrlHandler(Some(handler), 1);
            }
        });
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn install() {
        let _ = &super::COUNT;
    }
}

pub use imp::install;
