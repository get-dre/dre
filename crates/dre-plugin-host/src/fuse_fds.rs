//! Linux: a plugin must not inherit files on a FUSE mount into `execve`.
//!
//! `execve` closes the close-on-exec descriptors while it holds the lock that reading the
//! process's `/proc/<pid>/cmdline` or `environ` waits for. Closing a file on a FUSE mount sends
//! the mount's daemon a flush, and some daemons read the caller's `/proc` entries to answer it:
//! the daemon waits for `execve` and `execve` for the daemon, so the child sleeps for good before
//! the plugin starts. Databricks Unity Catalog Volumes (`/Volumes`) and workspace files
//! (`/Workspace`) on serverless compute are such mounts, and `dre run` keeps a Binding's `dre.log`
//! open in its run folder there whenever the target path is on one.
//!
//! So the child closes them itself, between `fork` and `execve`, when nothing holds that lock.
//! The parent's descriptors are untouched, and files on other filesystems still close at
//! `execve` as before.

use std::os::raw::c_int;
use std::process::Command;

/// Make `cmd`'s child close every descriptor it inherited on a FUSE mount before `execve`.
pub(crate) fn close_in_child(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the hook runs in the child between `fork` and `execve`, where only
    // async-signal-safe calls are allowed. `close_matching` and `is_fuse` make only `open`,
    // `getdents64`, `fstatfs` and `close` system calls on stack buffers: no allocation, no locks.
    unsafe {
        cmd.pre_exec(|| {
            close_matching(is_fuse);
            Ok(())
        });
    }
}

/// Whether `fd` is a file on a FUSE mount.
fn is_fuse(fd: c_int) -> bool {
    let mut st = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: `fstatfs` fills `st` and returns 0, or returns -1 and `st` isn't read.
    // 0x65735546 is FUSE_SUPER_MAGIC, as a literal so it takes `f_type`'s type on every libc.
    unsafe { libc::fstatfs(fd, st.as_mut_ptr()) == 0 && st.assume_init().f_type == 0x6573_5546 }
}

/// A buffer for `getdents64`, aligned for its records.
#[repr(C, align(8))]
struct DirentBuf([u8; 2048]);

/// Close every descriptor above stderr for which `matches` is true, listing `/proc/self/fd`
/// without allocating (safe between `fork` and `execve`). Does nothing without `/proc`.
fn close_matching(matches: fn(c_int) -> bool) {
    // SAFETY: the path is a NUL-terminated literal; the result is checked before use.
    let dir = unsafe {
        libc::open(
            c"/proc/self/fd".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if dir < 0 {
        return;
    }
    let mut buf = DirentBuf([0; 2048]);
    loop {
        // SAFETY: the kernel writes at most `buf.0.len()` bytes of `linux_dirent64` records into
        // `buf`, which lives for the call.
        let n = unsafe { libc::syscall(libc::SYS_getdents64, dir, buf.0.as_mut_ptr(), buf.0.len()) };
        let Ok(n) = usize::try_from(n) else { break };
        if n == 0 {
            break;
        }
        let records = buf.0.get(..n).unwrap_or_default();
        let mut at = 0;
        // A record: inode (8 bytes), offset (8), length (2), type (1), then the NUL-ended name.
        while let Some(&[lo, hi]) = records.get(at + 16..at + 18) {
            let len = usize::from(u16::from_ne_bytes([lo, hi]));
            let Some(name) = records.get(at + 19..at + len) else {
                break;
            };
            if let Some(fd) = parse_fd(name)
                && fd > 2
                && fd != dir
                && matches(fd)
            {
                // SAFETY: closes a descriptor of this (child) process; the parent's stays open.
                unsafe { libc::close(fd) };
            }
            at += len;
        }
    }
    // SAFETY: `dir` was opened above and is closed once.
    unsafe { libc::close(dir) };
}

/// A descriptor number from a `/proc/self/fd` entry's NUL-ended name (`.` and `..` aren't one).
fn parse_fd(name: &[u8]) -> Option<c_int> {
    let mut fd: c_int = 0;
    let mut digits = 0;
    for &b in name.iter().take_while(|&&b| b != 0) {
        if !b.is_ascii_digit() {
            return None;
        }
        fd = fd.checked_mul(10)?.checked_add(c_int::from(b - b'0'))?;
        digits += 1;
    }
    (digits > 0).then_some(fd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    use std::sync::atomic::{AtomicI32, Ordering};

    static TARGET: AtomicI32 = AtomicI32::new(-1);

    fn is_target(fd: c_int) -> bool {
        fd == TARGET.load(Ordering::SeqCst)
    }

    /// Whether a child shell still has `fd` open, with or without the hook closing it.
    fn child_sees(fd: c_int, close_it: bool) -> String {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            &format!("if [ -e /proc/self/fd/{fd} ]; then echo open; else echo closed; fi"),
        ]);
        if close_it {
            // SAFETY: the same async-signal-safe hook as `close_in_child`, with a test predicate.
            unsafe {
                cmd.pre_exec(|| {
                    close_matching(is_target);
                    Ok(())
                });
            }
        }
        let out = cmd.output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn closes_the_matching_descriptor_in_the_child_only() {
        let file = tempfile::tempfile().unwrap();
        let fd = file.as_raw_fd();
        // Inheritable, so without the hook the child keeps it.
        // SAFETY: clears FD_CLOEXEC on a descriptor this test owns.
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);
        TARGET.store(fd, Ordering::SeqCst);
        assert_eq!(child_sees(fd, false), "open");
        assert_eq!(child_sees(fd, true), "closed");
        // The parent's stays open.
        // SAFETY: F_GETFD only reads the descriptor's flags.
        assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
    }

    #[test]
    fn a_local_file_is_not_fuse() {
        let file = tempfile::tempfile().unwrap();
        assert!(!is_fuse(file.as_raw_fd()));
    }

    #[test]
    fn parses_descriptor_names() {
        assert_eq!(parse_fd(b"17\0"), Some(17));
        assert_eq!(parse_fd(b"3\0\0\0"), Some(3));
        assert_eq!(parse_fd(b".\0"), None);
        assert_eq!(parse_fd(b"..\0"), None);
        assert_eq!(parse_fd(b"\0"), None);
        assert_eq!(parse_fd(b"99999999999\0"), None);
    }

    #[test]
    fn plugins_start_with_the_hook() {
        let mut cmd = Command::new("true");
        close_in_child(&mut cmd);
        assert!(cmd.status().unwrap().success());
    }
}
