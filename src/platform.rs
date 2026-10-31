//! Exercise the blocking primitives supplied by the actual shipped executable.
use crate::Result;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Condvar, Mutex, mpsc},
    time::{Duration, Instant},
};

// Cosmopolitan 4.0.2 passes a signed int expectation to XNU's uint64_t API.
// Rust's thread parker uses u32::MAX, which must be zero-extended for a
// 32-bit comparison. Sign extension causes immediate returns and channel
// busy-spinning. Keep 64-bit operations intact. Remove when a pinned compiler
// fixes this ABI boundary and platform-smoke passes without the wrapper.
#[cfg(cosmo)]
unsafe extern "C" {
    fn __real_ulock_wait(
        op: u32,
        address: *mut std::ffi::c_void,
        value: u64,
        timeout: u32,
    ) -> std::ffi::c_int;
}
#[cfg(cosmo)]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __wrap_ulock_wait(
    op: u32,
    address: *mut std::ffi::c_void,
    value: u64,
    timeout: u32,
) -> std::ffi::c_int {
    let value = if matches!(op & 255, 1 | 3) {
        u64::from(value as u32)
    } else {
        value
    };
    // No logging here: the logger itself depends on these primitives.
    unsafe { __real_ulock_wait(op, address, value, timeout) }
}

/// APE Rust targets are Linux at build time, even when the loaded artifact is
/// running on macOS or Windows. Read the pinned Cosmopolitan runtime ABI instead.
pub fn host_os() -> &'static str {
    #[cfg(cosmo)]
    {
        unsafe extern "C" {
            static __hostos: std::ffi::c_int;
        }
        // Immutable after Cosmopolitan startup; values are defined by libc/dce.h.
        match unsafe { __hostos } {
            1 => "linux",
            2 => "baremetal",
            4 => "windows",
            8 => "macos",
            16 => "openbsd",
            32 => "freebsd",
            64 => "netbsd",
            _ => "unknown",
        }
    }
    #[cfg(not(cosmo))]
    {
        std::env::consts::OS
    }
}

pub fn check() -> Result<Value> {
    let pair = Arc::new((Mutex::new(false), Condvar::new()));
    let start = Instant::now();
    let budget = Duration::from_millis(200);
    let mut wakes = 0;
    let mut guard = pair.0.lock().unwrap();
    loop {
        let remaining = budget.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        let (next, result) = pair.1.wait_timeout(guard, remaining).unwrap();
        guard = next;
        wakes += 1;
        if result.timed_out() {
            break;
        }
        if wakes >= 100 {
            return Err(format!("condition-variable wait returned {wakes} times in {} ms without notification; blocking substrate is broken",start.elapsed().as_millis()).into());
        }
    }
    drop(guard);
    let condvar_ms = start.elapsed().as_millis();
    if condvar_ms < 150 {
        return Err("condition-variable timeout returned too early".into());
    }
    let notify = pair.clone();
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        *notify.0.lock().unwrap() = true;
        notify.1.notify_all();
    });
    let start = Instant::now();
    let mut guard = pair.0.lock().unwrap();
    let mut notifications = 0;
    while !*guard {
        let (next, result) = pair.1.wait_timeout(guard, Duration::from_secs(2)).unwrap();
        guard = next;
        notifications += 1;
        if result.timed_out() || notifications >= 100 {
            return Err(
                "condition-variable notification did not wake a blocking waiter correctly".into(),
            );
        }
    }
    let notify_ms = start.elapsed().as_millis();
    drop(guard);
    worker.join().map_err(|_| "notification worker panicked")?;
    let (sender, receiver) = mpsc::channel::<()>();
    let start = Instant::now();
    assert!(matches!(
        receiver.recv_timeout(budget),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    let channel_ms = start.elapsed().as_millis();
    drop(sender);
    if channel_ms < 150 {
        return Err("channel timeout returned too early".into());
    }
    Ok(
        json!({"os":host_os(),"architecture":std::env::consts::ARCH,"condition_variable_ms":condvar_ms,"condition_variable_wakes":wakes,"notification_ms":notify_ms,
        "notification_wakes":notifications,"channel_timeout_ms":channel_ms,"ok":true}),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn real_blocking_and_notification_primitives_work() {
        assert_eq!(super::check().unwrap()["ok"], true);
    }
}
