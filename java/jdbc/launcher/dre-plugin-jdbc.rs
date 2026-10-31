use std::{env, process::Command};

#[cfg(windows)]
fn runtime_job() -> std::io::Result<std::os::windows::io::OwnedHandle> {
    use std::ffi::c_void;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

    #[repr(C)]
    #[derive(Default)]
    struct BasicLimits {
        process_time: i64,
        job_time: i64,
        flags: u32,
        min_working_set: usize,
        max_working_set: usize,
        active_processes: u32,
        affinity: usize,
        priority: u32,
        scheduling: u32,
    }
    #[repr(C)]
    #[derive(Default)]
    struct ExtendedLimits {
        basic: BasicLimits,
        io_counters: [u64; 6],
        process_memory: usize,
        job_memory: usize,
        peak_process_memory: usize,
        peak_job_memory: usize,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(job: *mut c_void, class: i32, info: *const c_void, size: u32) -> i32;
        fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
    }
    // Assign the launcher before spawning Java: descendants inherit the job without a race.
    // The non-inherited handle closes even when the host forcibly kills this launcher.
    // SAFETY: repr(C) structs match Win32's limits layout. Pointers reference live stack data
    // for synchronous calls. A non-null job handle is owned exactly once; the pseudo process
    // handle is borrowed and never closed. Null creation arguments request default security.
    unsafe {
        let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(raw);
        let mut limits = ExtendedLimits::default();
        limits.basic.flags = 0x00002000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        if SetInformationJobObject(
            job.as_raw_handle(),
            9, // JobObjectExtendedLimitInformation
            (&limits as *const ExtendedLimits).cast(),
            std::mem::size_of::<ExtendedLimits>() as u32,
        ) == 0
            || AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }
}

fn main() {
    #[cfg(windows)]
    let _job = runtime_job().unwrap_or_else(|error| {
        eprintln!("can't manage the bundled JDBC runtime process: {error}");
        std::process::exit(1);
    });
    let Ok(executable) = env::current_exe() else {
        eprintln!("can't find the JDBC launcher executable");
        std::process::exit(1);
    };
    let Some(directory) = executable.parent() else {
        eprintln!("the JDBC launcher has no parent directory");
        std::process::exit(1);
    };
    let mut child = Command::new(directory.join("runtime").join("bin").join("java.exe"))
        .arg("--add-opens=java.base/java.nio=ALL-UNNAMED")
        .arg("-jar")
        .arg(directory.join("dre-plugin-jdbc.jar"))
        .args(env::args_os().skip(1))
        .spawn()
        .unwrap_or_else(|error| {
            eprintln!("can't start the bundled JDBC runtime: {error}");
            std::process::exit(1);
        });
    let status = child.wait().unwrap_or_else(|error| {
        eprintln!("can't wait for the bundled JDBC runtime: {error}");
        std::process::exit(1);
    });
    std::process::exit(status.code().unwrap_or(1));
}
