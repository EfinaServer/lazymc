#[cfg(windows)]
pub mod windows;

#[cfg(unix)]
use nix::{
    sys::signal::{self, Signal},
    unistd::Pid,
};

/// Force kill process.
///
/// Results in undefined behavior if PID is invalid.
#[allow(unreachable_code)]
pub fn force_kill(pid: u32) -> bool {
    #[cfg(unix)]
    return unix_signal(pid, Signal::SIGKILL);

    #[cfg(windows)]
    unsafe {
        return windows::force_kill(pid);
    }

    unimplemented!("force killing Minecraft server process not implemented on this platform");
}

/// Gracefully kill process.
/// Results in undefined behavior if PID is invalid.
///
/// # Panics
/// Panics on platforms other than Unix.
#[allow(unreachable_code, dead_code, unused_variables)]
pub fn kill_gracefully(pid: u32) -> bool {
    #[cfg(unix)]
    return unix_signal(pid, Signal::SIGTERM);

    unimplemented!(
        "gracefully killing Minecraft server process not implemented on non-Unix platforms"
    );
}

/// Freeze process.
/// Results in undefined behavior if PID is invaild.
///
/// # Panics
/// Panics on platforms other than Unix.
#[allow(unreachable_code)]
pub fn freeze(pid: u32) -> bool {
    #[cfg(unix)]
    return unix_signal(pid, Signal::SIGSTOP);

    unimplemented!(
        "freezing the Minecraft server process is not implemented on non-Unix platforms"
    );
}

/// Unfreeze process.
/// Results in undefined behavior if PID is invaild.
///
/// # Panics
/// Panics on platforms other than Unix.
#[allow(unreachable_code)]
pub fn unfreeze(pid: u32) -> bool {
    #[cfg(unix)]
    return unix_signal(pid, Signal::SIGCONT);

    unimplemented!(
        "unfreezing the Minecraft server process is not implemented on non-Unix platforms"
    );
}

#[cfg(unix)]
pub fn unix_signal(pid: u32, signal: Signal) -> bool {
    // Send signal to the process group (negative PID) so all child processes
    // receive it. This is critical for modded servers launched via wrapper scripts,
    // where the direct PID is the shell and Java runs as a child process.
    let pgid = -(pid as i32);
    match signal::kill(Pid::from_raw(pgid), signal) {
        Ok(()) => true,
        Err(_) => {
            // Fallback to sending directly to the process if process group signal fails
            debug!(target: "lazymc", "Process group signal {signal} failed, trying direct PID");
            match signal::kill(Pid::from_raw(pid as i32), signal) {
                Ok(()) => true,
                Err(err) => {
                    warn!(target: "lazymc", "Sending {signal} signal to server failed: {err}");
                    false
                }
            }
        }
    }
}
