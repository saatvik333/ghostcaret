//! Installed RAM, and memory-pressure notifications from the OS.

/// Total physical memory in bytes, when the OS reports it.
#[cfg(unix)]
pub fn total_bytes() -> Option<u64> {
    // SAFETY: sysconf has no preconditions.
    let (pages, page) = unsafe { (libc::sysconf(libc::_SC_PHYS_PAGES), libc::sysconf(libc::_SC_PAGESIZE)) };
    (pages > 0 && page > 0).then(|| pages as u64 * page as u64)
}

#[cfg(windows)]
pub fn total_bytes() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX { dwLength: size_of::<MEMORYSTATUSEX>() as u32, ..Default::default() };
    // SAFETY: `status` is a valid MEMORYSTATUSEX with dwLength set.
    (unsafe { GlobalMemoryStatusEx(&mut status) } != 0).then_some(status.ullTotalPhys)
}

/// Calls `on_pressure` from a background thread whenever the OS reports that
/// memory is short. The thread sleeps in the kernel between reports.
#[cfg(target_os = "linux")]
pub fn watch_pressure(on_pressure: impl Fn() + Send + 'static) {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;

    // A PSI trigger: wake when tasks stall on memory for 150 ms within a 2 s
    // window. Unprivileged triggers need a window that is a multiple of 2 s.
    let file = OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open("/proc/pressure/memory");
    let mut file = match file {
        Ok(f) => f,
        Err(e) => {
            log::info!("memory pressure notifications unavailable: {e}");
            return;
        }
    };
    if let Err(e) = file.write_all(b"some 150000 2000000\0") {
        log::info!("memory pressure trigger refused: {e}");
        return;
    }
    let spawned = std::thread::Builder::new().name("oft-mempressure".into()).spawn(move || {
        let mut fds = libc::pollfd { fd: file.as_raw_fd(), events: libc::POLLPRI, revents: 0 };
        loop {
            // SAFETY: `fds` points to one valid pollfd for the open file.
            let n = unsafe { libc::poll(&mut fds, 1, -1) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if fds.revents & libc::POLLERR != 0 {
                return;
            }
            if fds.revents & libc::POLLPRI != 0 {
                on_pressure();
            }
        }
    });
    if let Err(e) = spawned {
        log::warn!("memory pressure thread failed to start: {e}");
    }
}

#[cfg(windows)]
pub fn watch_pressure(on_pressure: impl Fn() + Send + 'static) {
    use windows_sys::Win32::System::Memory::{CreateMemoryResourceNotification, LowMemoryResourceNotification};
    use windows_sys::Win32::System::Threading::{INFINITE, WaitForSingleObject};

    // SAFETY: plain constructor call; a null handle signals failure.
    let handle = unsafe { CreateMemoryResourceNotification(LowMemoryResourceNotification) };
    if handle.is_null() {
        log::info!("memory pressure notifications unavailable");
        return;
    }
    let handle = handle as usize;
    let spawned = std::thread::Builder::new().name("oft-mempressure".into()).spawn(move || {
        loop {
            // SAFETY: the handle stays open for the life of the process.
            unsafe { WaitForSingleObject(handle as _, INFINITE) };
            on_pressure();
            // The object stays signalled while memory is low; there is nothing
            // more to free, so look again only after a while.
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    });
    if let Err(e) = spawned {
        log::warn!("memory pressure thread failed to start: {e}");
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub fn watch_pressure(_on_pressure: impl Fn() + Send + 'static) {}

#[cfg(test)]
mod tests {
    #[test]
    fn reports_some_memory() {
        let total = super::total_bytes().expect("total memory");
        assert!(total > 256 << 20);
    }
}
