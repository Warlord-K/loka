use tracing::info;

const RESERVED_MB: u64 = 384;
const WARM_SESSION_MB: u64 = 220;
const WORK_RUN_MB: u64 = 1536;

#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub cpus: usize,
    pub memory_mb: u64,
    pub chat_slots: usize,
    pub work_slots: usize,
    pub warm_sessions: usize,
}

impl Budget {
    pub fn detect() -> Self {
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let memory_mb = memory_limit_mb().unwrap_or(4096);
        let budget = Self::for_machine(cpus, memory_mb).with_overrides();
        info!(
            cpus = budget.cpus,
            memory_mb = budget.memory_mb,
            chat_slots = budget.chat_slots,
            work_slots = budget.work_slots,
            warm_sessions = budget.warm_sessions,
            "Resource budget"
        );
        budget
    }

    pub fn for_machine(cpus: usize, memory_mb: u64) -> Self {
        let usable = memory_mb.saturating_sub(RESERVED_MB);
        let warm_sessions = (usable * 3 / 10 / WARM_SESSION_MB).clamp(1, 48) as usize;
        let work_slots = (cpus / 2).min((usable * 4 / 10 / WORK_RUN_MB) as usize).clamp(1, 12);
        let chat_slots = warm_sessions.min(cpus.max(1) * 2).clamp(1, 24);
        Self { cpus, memory_mb, chat_slots, work_slots, warm_sessions }
    }

    fn with_overrides(mut self) -> Self {
        let read = |key: &str| std::env::var(key).ok().and_then(|v| v.trim().parse::<usize>().ok());
        if let Some(n) = read("WORLD_INBOX_CONCURRENCY").filter(|n| *n > 0) {
            self.chat_slots = n;
        }
        if let Some(n) = read("WORLD_RUN_CONCURRENCY").filter(|n| *n > 0) {
            self.work_slots = n;
        }
        match read("WORLD_WARM_SESSIONS") {
            Some(n) => self.warm_sessions = n,
            None => self.warm_sessions = self.warm_sessions.max(self.chat_slots),
        }
        self
    }
}

fn memory_limit_mb() -> Option<u64> {
    let physical = physical_memory_bytes();
    let limit = cgroup_limit_bytes();
    let bytes = match (physical, limit) {
        (Some(p), Some(l)) => p.min(l),
        (p, l) => p.or(l)?,
    };
    Some(bytes / (1024 * 1024))
}

fn cgroup_limit_bytes() -> Option<u64> {
    ["/sys/fs/cgroup/memory.max", "/sys/fs/cgroup/memory/memory.limit_in_bytes"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| text.trim().parse::<u64>().ok())
        .find(|bytes| *bytes > 0 && *bytes < (1 << 50))
}

#[cfg(target_os = "linux")]
fn physical_memory_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

#[cfg(target_os = "macos")]
fn physical_memory_bytes() -> Option<u64> {
    let output = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output().ok()?;
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn physical_memory_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::Budget;

    #[test]
    fn tiny_cloud_instance_runs_one_of_each() {
        let b = Budget::for_machine(1, 512);
        assert_eq!((b.chat_slots, b.work_slots, b.warm_sessions), (1, 1, 1));
    }

    #[test]
    fn two_gigabytes_holds_a_few_warm_agents() {
        let b = Budget::for_machine(1, 2048);
        assert_eq!(b.work_slots, 1);
        assert_eq!(b.warm_sessions, 2);
        assert_eq!(b.chat_slots, 2);
    }

    #[test]
    fn eight_gigabyte_server_handles_many_simple_agents() {
        let b = Budget::for_machine(4, 8192);
        assert_eq!(b.warm_sessions, 10);
        assert_eq!(b.chat_slots, 8);
        assert_eq!(b.work_slots, 2);
    }

    #[test]
    fn laptop_keeps_room_for_the_person_using_it() {
        let b = Budget::for_machine(11, 18432);
        assert_eq!(b.warm_sessions, 24);
        assert_eq!(b.chat_slots, 22);
        assert_eq!(b.work_slots, 4);
    }
}
