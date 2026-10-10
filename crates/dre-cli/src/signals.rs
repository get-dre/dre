//! Clean cancellation of `dre run`: Ctrl-C (SIGINT), termination (SIGTERM; on Windows
//! Ctrl-Break, closing the console, logging off, shutting down) and the run's timeout.
//!
//! On the first signal (or when the timeout runs out) the run's [`CancelToken`] is cancelled (no further Binding, statement or
//! delivery starts) and every running plugin is sent `cancel`. Plugins get 8 seconds to stop,
//! then are killed; 2 seconds later, if the run still hasn't finished, `dre` exits anyway, so the
//! whole stop fits Docker's 10-second grace period. A second Ctrl-C stops at once. The exit code
//! is 130 after Ctrl-C, 143 after a termination and 124 after a timeout.
//!
//! A signal handler may only do async-signal-safe work, so it just records the signal; a watcher
//! thread acts on it.

use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use dre_core::engine::{CancelReason, CancelToken};

/// How long plugins get to stop before they're killed.
const PLUGIN_GRACE: Duration = Duration::from_secs(8);
/// How long after that `dre` waits for the run to write its results before exiting anyway.
const CORE_GRACE: Duration = Duration::from_secs(2);

/// The last signal: 0 none, 1 interrupt, 2 terminate.
static LAST: AtomicU8 = AtomicU8::new(0);
/// How many signals have arrived.
static COUNT: AtomicU32 = AtomicU32::new(0);

fn record(reason: CancelReason) {
    LAST.store(
        match reason {
            CancelReason::Terminate => 2,
            _ => 1,
        },
        Ordering::SeqCst,
    );
    COUNT.fetch_add(1, Ordering::SeqCst);
}

/// The run's timeout in milliseconds (0: none), set once it's known (see [`set_timeout`]).
static TIMEOUT_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Time the run: cancel it when `timeout` (counted from [`watch`]) runs out.
pub fn set_timeout(timeout: Option<Duration>) {
    TIMEOUT_MS.store(
        timeout.map_or(0, |t| t.as_millis().max(1) as u64),
        Ordering::SeqCst,
    );
}

/// Catch the signals from now on, for the rest of the process: cancel `cancel` on the first one
/// or when the timeout set with [`set_timeout`] runs out. Call it first thing, so a signal that
/// arrives while the project loads isn't lost.
pub fn watch(cancel: CancelToken) {
    install();
    let started = Instant::now();
    std::thread::spawn(move || {
        let mut seen = 0;
        let mut first: Option<(Instant, CancelReason)> = None;
        let mut killed = false;
        loop {
            std::thread::sleep(Duration::from_millis(50));
            let ms = TIMEOUT_MS.load(Ordering::SeqCst);
            let timeout = (ms > 0).then(|| Duration::from_millis(ms));
            if first.is_none()
                && let Some(t) = timeout
                && started.elapsed() >= t
            {
                first = Some((Instant::now(), CancelReason::Timeout));
                cancel.cancel_with(CancelReason::Timeout);
                eprintln!(
                    "\nTimed out after {}: stopping the running plugins",
                    crate::signals::human(t)
                );
                dre_protocol::host::cancel_all();
            }
            let count = COUNT.load(Ordering::SeqCst);
            if count > seen {
                seen = count;
                let reason = match LAST.load(Ordering::SeqCst) {
                    2 => CancelReason::Terminate,
                    _ => CancelReason::Interrupt,
                };
                match first {
                    None => {
                        first = Some((Instant::now(), reason));
                        cancel.cancel_with(reason);
                        eprintln!(
                            "\nCancelling: stopping the running plugins (press Ctrl-C again to stop at once)"
                        );
                        dre_protocol::host::cancel_all();
                    }
                    Some((_, r)) if reason == CancelReason::Interrupt => {
                        dre_protocol::host::kill_all();
                        std::process::exit(i32::from(r.exit_code()));
                    }
                    Some(_) => {}
                }
            }
            if let Some((at, reason)) = first {
                let waited = at.elapsed();
                if !killed && waited >= PLUGIN_GRACE {
                    killed = true;
                    dre_protocol::host::kill_all();
                }
                if waited >= PLUGIN_GRACE + CORE_GRACE {
                    std::process::exit(i32::from(reason.exit_code()));
                }
            }
        }
    });
}

/// `2h`, `90m`, `45s`.
pub fn human(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        _ if s >= 3600 && s.is_multiple_of(3600) => format!("{}h", s / 3600),
        _ if s >= 60 && s.is_multiple_of(60) => format!("{}m", s / 60),
        _ => format!("{s}s"),
    }
}

#[cfg(unix)]
fn install() {
    extern "C" fn on_signal(sig: libc::c_int) {
        record(if sig == libc::SIGTERM {
            CancelReason::Terminate
        } else {
            CancelReason::Interrupt
        });
    }
    // SAFETY: the handler only stores to atomics, which is async-signal-safe.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        for sig in [libc::SIGINT, libc::SIGTERM] {
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

#[cfg(windows)]
fn install() {
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    };
    unsafe extern "system" fn on_event(kind: u32) -> windows_sys::core::BOOL {
        match kind {
            CTRL_C_EVENT => record(CancelReason::Interrupt),
            CTRL_BREAK_EVENT => record(CancelReason::Terminate),
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                record(CancelReason::Terminate);
                // Windows ends the process when this returns: give the run its grace first.
                std::thread::sleep(PLUGIN_GRACE + CORE_GRACE);
            }
            _ => return 0,
        }
        1
    }
    // SAFETY: registering a handler that only records the event (and, for the closing events,
    // waits); it lives for the whole process.
    unsafe {
        SetConsoleCtrlHandler(Some(on_event), 1);
    }
}
