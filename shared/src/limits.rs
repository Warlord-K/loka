use tracing::{info, warn};

const WANTED_OPEN_FILES: u64 = 65536;
const MACOS_OPEN_MAX: u64 = 10240;

pub fn raise_open_files() -> Option<u64> {
    let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        warn!("Could not read the open files limit");
        return None;
    }
    let before: u64 = limit.rlim_cur;
    let wanted = WANTED_OPEN_FILES.min(limit.rlim_max);
    if before >= wanted {
        return Some(before);
    }
    for target in [wanted, wanted.min(MACOS_OPEN_MAX)] {
        let raised = libc::rlimit { rlim_cur: target as libc::rlim_t, rlim_max: limit.rlim_max };
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } == 0 {
            info!(before, after = target, "Raised open files limit");
            return Some(target);
        }
    }
    warn!(limit = before, "Could not raise open files limit");
    Some(before)
}

pub fn hide_process_memory() -> bool {
    #[cfg(target_os = "linux")]
    {
        let ok = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } == 0;
        if !ok {
            warn!("Could not mark the process non-dumpable; child processes of the same user can read its environment");
        }
        ok
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

pub fn open_files() -> Option<usize> {
    let entries = std::fs::read_dir("/dev/fd").ok()?;
    Some(entries.count().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_memory_can_be_hidden() {
        assert!(hide_process_memory());
    }

    #[test]
    fn raises_a_low_open_files_limit() {
        let mut limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
        let low = libc::rlimit { rlim_cur: 256, rlim_max: limit.rlim_max };
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &low) }, 0);
        let raised = raise_open_files().unwrap();
        assert!(raised > 256, "still {raised}");
        assert!(open_files().unwrap() > 0);
    }
}
