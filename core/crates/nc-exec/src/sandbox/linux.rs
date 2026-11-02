//! Linux sandbox. The trusted worker bootstrap finishes isolation before acknowledging spawn.
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::{ChildPipes, SandboxError, SandboxEvent, SandboxSpec, SetupTimings};

/// Prefix of private cgroups; owner PID and start time prevent sweeping live owners.
pub const PROFILE_PREFIX: &str = "nc-exec-";
const BOOTSTRAP: &str = "--nc-linux-sandbox-v1";
const PIDS: &str = "32";
#[cfg(test)]
thread_local! { static REFUSE: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) }; }

fn error(step: &'static str, e: std::io::Error) -> SandboxError {
    SandboxError {
        step,
        // Strip any path context added by an I/O wrapper (notably tempfile).
        detail: std::io::Error::from_raw_os_error(e.raw_os_error().unwrap_or(libc::EIO))
            .to_string(),
    }
}
fn os(step: &'static str) -> SandboxError {
    error(step, std::io::Error::last_os_error())
}
fn check(step: &'static str, result: i32) -> Result<(), SandboxError> {
    if result == -1 {
        Err(os(step))
    } else {
        Ok(())
    }
}
fn invalid(step: &'static str) -> SandboxError {
    error(step, std::io::Error::from_raw_os_error(libc::EINVAL))
}
fn duplicate_private(fd: i32, step: &'static str) -> Result<OwnedFd, SandboxError> {
    // SAFETY: fd is borrowed from a live owned descriptor. F_DUPFD_CLOEXEC creates a new descriptor
    // above stdio/control fd 3, so Command's stdio setup cannot overwrite it before pre_exec.
    let copy = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 4) };
    if copy == -1 {
        return Err(os(step));
    }
    // SAFETY: copy is a newly allocated descriptor owned exactly once by the returned OwnedFd.
    Ok(unsafe { OwnedFd::from_raw_fd(copy) })
}
fn start_time(pid: u32) -> Option<String> {
    let s = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    s.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}
fn cgroup_base() -> Result<PathBuf, SandboxError> {
    let membership = fs::read_to_string("/proc/self/cgroup").map_err(|e| error("cgroup", e))?;
    let relative = membership
        .lines()
        .find_map(|s| s.strip_prefix("0::/"))
        .ok_or_else(|| invalid("cgroup"))?;
    if Path::new(relative)
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err(invalid("cgroup"));
    }
    use std::os::unix::fs::MetadataExt;
    let mut base = Path::new("/sys/fs/cgroup").join(relative);
    loop {
        let enabled = fs::read_to_string(base.join("cgroup.subtree_control")).unwrap_or_default();
        // Only use a delegated subtree owned by the invoking user. Do not mutate its controller policy.
        // SAFETY: getuid has no arguments and does not change process state.
        let owned = fs::metadata(&base)
            .map(|m| m.uid() == unsafe { libc::getuid() })
            .unwrap_or(false);
        if owned
            && ["memory", "pids", "cpu"]
                .iter()
                .all(|c| enabled.split_whitespace().any(|v| v == *c))
        {
            return Ok(base);
        }
        if base == Path::new("/sys/fs/cgroup") || !base.pop() {
            break;
        }
    }
    Err(error(
        "cgroup",
        std::io::Error::from_raw_os_error(libc::EACCES),
    ))
}

#[derive(Debug)]
struct Group(PathBuf);
impl Group {
    fn create(memory: u64) -> Result<Self, SandboxError> {
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| invalid("cgroup"))?;
        let owner = start_time(std::process::id()).ok_or_else(|| invalid("cgroup"))?;
        let path = cgroup_base()?.join(format!(
            "{PROFILE_PREFIX}{}-{owner}-{}",
            std::process::id(),
            hex::encode(random)
        ));
        fs::create_dir(&path).map_err(|e| error("cgroup", e))?;
        let group = Self(path);
        for (key, value) in [
            ("memory.max", memory.to_string()),
            ("memory.swap.max", "0".into()),
            ("memory.oom.group", "1".into()),
            ("pids.max", PIDS.into()),
            ("cpu.max", "100000 100000".into()),
        ] {
            fs::write(group.0.join(key), value).map_err(|e| error("cgroup", e))?;
        }
        // Require whole-tree kill support before starting anything.
        fs::OpenOptions::new()
            .write(true)
            .open(group.0.join("cgroup.kill"))
            .map_err(|e| error("cgroup", e))?;
        Ok(group)
    }
    fn kill(&self) {
        let _ = fs::write(self.0.join("cgroup.kill"), "1");
    }
    fn remove(&self) {
        self.kill();
        let until = Instant::now() + Duration::from_millis(800);
        loop {
            match fs::remove_dir(&self.0) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) if Instant::now() < until => std::thread::sleep(Duration::from_millis(2)),
                Err(_) => return, // A crashed/unreaped owner is handled by the startup sweep.
            }
        }
    }
    fn value(&self, file: &str, key: &str) -> u64 {
        fs::read_to_string(self.0.join(file))
            .ok()
            .and_then(|s| {
                s.lines().find_map(|l| {
                    let (k, v) = l.split_once(' ')?;
                    (k == key).then(|| v.parse().ok()).flatten()
                })
            })
            .unwrap_or(0)
    }
}
impl Drop for Group {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Worker and its entire cgroup. Drop kills, reaps and removes all resources.
pub struct SandboxedChild {
    process: Child,
    group: Option<Group>,
    root: Option<tempfile::TempDir>,
    gate: Option<UnixStream>,
    timings: SetupTimings,
    exit: Option<u32>,
    peak: AtomicU64,
    memory_events: u64,
    pids_events: u64,
    sigsys_reported: bool,
}
impl std::fmt::Debug for SandboxedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedChild").finish_non_exhaustive()
    }
}
impl SandboxedChild {
    pub(super) fn spawn(spec: &SandboxSpec) -> Result<(Self, ChildPipes), SandboxError> {
        #[cfg(test)]
        if let Some(step) = REFUSE.with(|r| r.get()) {
            return Err(error(step, std::io::Error::from_raw_os_error(libc::EPERM)));
        }
        // SAFETY: getuid only observes credentials. Root callers are outside this unprivileged API.
        if unsafe { libc::getuid() } == 0 {
            return Err(error(
                "userns",
                std::io::Error::from_raw_os_error(libc::EPERM),
            ));
        }
        let t = Instant::now();
        let group = Group::create(spec.process_memory_bytes)?;
        let root_prefix = format!(
            "{}-",
            group
                .0
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| invalid("mount"))?
        );
        let root = tempfile::Builder::new()
            .prefix(&root_prefix)
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .map_err(|e| error("mount", e))?;
        let profile = t.elapsed();
        let launch = Instant::now();
        let cgroup_procs = {
            let file = fs::OpenOptions::new()
                .write(true)
                .open(group.0.join("cgroup.procs"))
                .map_err(|e| error("cgroup", e))?;
            File::from(duplicate_private(file.as_raw_fd(), "cgroup")?)
        };
        let (mut gate, original_gate) = UnixStream::pair().map_err(|e| error("pipes", e))?;
        let child_gate = UnixStream::from(duplicate_private(original_gate.as_raw_fd(), "pipes")?);
        drop(original_gate);
        gate.set_read_timeout(Some(Duration::from_secs(10)))
            .map_err(|e| error("pipes", e))?;
        let fd = child_gate.as_raw_fd();
        let cgroup_fd = cgroup_procs.as_raw_fd();
        let mut command = Command::new(&spec.worker);
        command
            .arg(BOOTSTRAP)
            .arg(root.path())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: the closure calls only async-signal-safe descriptor syscalls after fork. fd remains
        // owned by child_gate until spawn returns; dup2 and fcntl reserve fd 3 for the bootstrap.
        unsafe {
            command.pre_exec(move || {
                // Writing 0 moves the calling child before exec, so loader/startup memory is capped too.
                if libc::write(cgroup_fd, b"0".as_ptr().cast(), 1) != 1 {
                    let failure = std::io::Error::last_os_error();
                    let _ = libc::write(fd, b"C".as_ptr().cast(), 1);
                    return Err(failure);
                }
                if fd != 3 && libc::dup2(fd, 3) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let launched = command.spawn();
        drop((child_gate, cgroup_procs));
        let process = match launched {
            Ok(process) => process,
            Err(e) => {
                let mut marker = [0];
                let step = if matches!(gate.read(&mut marker), Ok(1)) && marker[0] == b'C' {
                    "cgroup"
                } else {
                    "launch"
                };
                return Err(error(step, e));
            }
        };
        let mut child = Self {
            process,
            group: Some(group),
            root: Some(root),
            gate: None,
            timings: SetupTimings {
                profile,
                launch: Duration::ZERO,
            },
            exit: None,
            peak: AtomicU64::new(0),
            memory_events: 0,
            pids_events: 0,
            sigsys_reported: false,
        };
        gate.write_all(&[1]).map_err(|e| error("bootstrap", e))?;
        let mut reply = [0; 8];
        gate.read_exact(&mut reply)
            .map_err(|e| error("bootstrap", e))?;
        if reply[0] != 0 {
            let step = match reply[0] {
                1 => "userns",
                2 => "namespaces",
                3 => "mount",
                4 => "capabilities",
                5 => "seccomp",
                _ => "bootstrap",
            };
            return Err(error(
                step,
                std::io::Error::from_raw_os_error(i32::from_ne_bytes(
                    reply[4..8].try_into().map_err(|_| invalid(step))?,
                )),
            ));
        }
        child.gate = Some(gate);
        child.timings.launch = launch.elapsed();
        let stdin: OwnedFd = child
            .process
            .stdin
            .take()
            .ok_or_else(|| invalid("pipes"))?
            .into();
        let stdout: OwnedFd = child
            .process
            .stdout
            .take()
            .ok_or_else(|| invalid("pipes"))?
            .into();
        let stderr: OwnedFd = child
            .process
            .stderr
            .take()
            .ok_or_else(|| invalid("pipes"))?
            .into();
        Ok((
            child,
            ChildPipes {
                stdin: File::from(stdin),
                stdout: File::from(stdout),
                stderr: File::from(stderr),
            },
        ))
    }
    /// Release the worker only after isolation is complete.
    pub fn resume(&mut self) -> Result<(), SandboxError> {
        if let Some(mut gate) = self.gate.take() {
            gate.write_all(&[1]).map_err(|e| error("resume", e))?;
        }
        Ok(())
    }
    /// Wait at most timeout. Signal exits are represented as 128 + signal.
    pub fn wait(&mut self, timeout: Duration) -> Option<u32> {
        let until = Instant::now() + timeout;
        loop {
            let _ = self.peak_memory_bytes();
            if let Some(code) = self.exit {
                return Some(code);
            }
            if let Ok(Some(status)) = self.process.try_wait() {
                let code = status
                    .code()
                    .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
                    as u32;
                self.exit = Some(code);
                return self.exit;
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    /// Poll cgroup counters and seccomp termination, reporting each transition once.
    pub fn events(&mut self) -> Vec<SandboxEvent> {
        let mut out = Vec::new();
        if let Some(group) = &self.group {
            let memory = group.value("memory.events", "oom_kill");
            let pids = group.value("pids.events", "max");
            if memory > self.memory_events {
                out.push(SandboxEvent::MemoryLimit);
            }
            if pids > self.pids_events {
                out.push(SandboxEvent::ProcessLimit);
            }
            self.memory_events = memory;
            self.pids_events = pids;
        }
        let _ = self.wait(Duration::ZERO);
        if self.exit == Some((128 + libc::SIGSYS) as u32) && !self.sigsys_reported {
            self.sigsys_reported = true;
            out.push(SandboxEvent::ProcessLimit);
        }
        out
    }
    /// Immediately kill every process in the cgroup.
    pub fn kill(&mut self) {
        if let Some(group) = &self.group {
            group.kill();
        }
    }
    /// Setup measurements.
    pub fn timings(&self) -> SetupTimings {
        self.timings
    }
    /// Kernel-accounted peak, including descendants and charged tmpfs pages.
    pub fn peak_memory_bytes(&self) -> u64 {
        let read = |name| {
            self.group
                .as_ref()
                .and_then(|g| fs::read_to_string(g.0.join(name)).ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .unwrap_or(0)
        };
        // memory.peak is unavailable on older cgroup v2 kernels. wait/events poll current as well,
        // preserving the highest observed charge without weakening the kernel-enforced cap.
        let value = read("memory.peak").max(read("memory.current"));
        self.peak.fetch_max(value, Ordering::Relaxed).max(value)
    }
    /// Kill, reap and remove the private cgroup and staging directory.
    pub fn teardown(mut self) -> Duration {
        self.cleanup()
    }
    fn cleanup(&mut self) -> Duration {
        let t = Instant::now();
        self.kill();
        // Also kill the trusted launcher if cgroup attachment failed during setup.
        let _ = self.process.kill();
        let _ = self.process.wait();
        self.group.take();
        self.root.take();
        t.elapsed()
    }
}
impl Drop for SandboxedChild {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Remove only this user's stale sandbox cgroups; PID start time also guards PID reuse.
pub fn sweep_leftovers() -> Result<usize, SandboxError> {
    let mut removed = 0;
    for entry in fs::read_dir(cgroup_base()?).map_err(|e| error("cgroup", e))? {
        let entry = entry.map_err(|e| error("cgroup", e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str().and_then(|n| n.strip_prefix(PROFILE_PREFIX)) else {
            continue;
        };
        let mut parts = name.split('-');
        let (Some(pid), Some(start), Some(nonce), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(pid) = pid.parse() else { continue };
        if nonce.len() != 32 || !nonce.bytes().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        if start_time(pid).as_deref() == Some(start) {
            continue;
        }
        let group = Group(entry.path());
        group.remove();
        if group.0.exists() {
            return Err(error(
                "sweep",
                std::io::Error::from_raw_os_error(libc::EBUSY),
            ));
        }
        sweep_staging(&format!("{PROFILE_PREFIX}{name}-"))?;
        removed += 1;
    }
    Ok(removed)
}

fn sweep_staging(prefix: &str) -> Result<(), SandboxError> {
    use std::os::unix::fs::MetadataExt;
    for entry in fs::read_dir(std::env::temp_dir()).map_err(|e| error("sweep", e))? {
        let entry = entry.map_err(|e| error("sweep", e))?;
        if !entry.file_name().to_string_lossy().starts_with(prefix) {
            continue;
        }
        let metadata = entry.metadata().map_err(|e| error("sweep", e))?;
        // SAFETY: getuid only reads the caller's credentials.
        let uid = unsafe { libc::getuid() };
        // Never recurse: the host mountpoint is empty because worker writes are in its private namespace.
        if metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o777 == 0o700 {
            fs::remove_dir(entry.path()).map_err(|e| error("sweep", e))?;
        }
    }
    Ok(())
}

fn cstring(path: &Path) -> Result<std::ffi::CString, SandboxError> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| invalid("mount"))
}
fn mount(
    source: Option<&Path>,
    target: &Path,
    kind: Option<&std::ffi::CStr>,
    flags: libc::c_ulong,
) -> Result<(), SandboxError> {
    let source = source.map(cstring).transpose()?;
    let target = cstring(target)?;
    // SAFETY: all optional C strings outlive this syscall; null data means no filesystem options.
    check("mount", unsafe {
        libc::mount(
            source.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
            target.as_ptr(),
            kind.map_or(std::ptr::null(), |s| s.as_ptr()),
            flags,
            std::ptr::null(),
        )
    })
}
/// Flags a user namespace may not clear on remount: the kernel locks the source mount's
/// noexec and atime settings, and a bind remount that omits them fails with EPERM.
fn locked_flags(path: &Path) -> Result<libc::c_ulong, SandboxError> {
    let path = cstring(path)?;
    // SAFETY: zeroed statvfs is a valid out-buffer; path is a valid C string for the call.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: see above; the kernel writes only into `st`.
    check("mount", unsafe { libc::statvfs(path.as_ptr(), &mut st) })?;
    let mut flags = 0;
    for (st_flag, ms_flag) in [
        (libc::ST_NOEXEC, libc::MS_NOEXEC),
        (libc::ST_NOATIME, libc::MS_NOATIME),
        (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
        (libc::ST_RELATIME, libc::MS_RELATIME),
    ] {
        if st.f_flag & st_flag != 0 {
            flags |= ms_flag;
        }
    }
    Ok(flags)
}
fn filesystem(root: &Path) -> Result<(), SandboxError> {
    // Snapshot only already-loaded executable/library mappings; never execute ldd or mount directories.
    let maps = fs::read_to_string("/proc/self/maps").map_err(|e| error("mount", e))?;
    let worker = fs::read_link("/proc/self/exe").map_err(|e| error("mount", e))?;
    let mut files = std::collections::BTreeSet::new();
    for line in maps.lines() {
        if let Some(path) = line.find('/').map(|start| &line[start..]) {
            files.insert(PathBuf::from(path));
        }
    }
    mount(None, Path::new("/"), None, libc::MS_REC | libc::MS_PRIVATE)?;
    mount(None, root, Some(c"tmpfs"), libc::MS_NOSUID | libc::MS_NODEV)?;
    for source in files {
        // Loaded libraries need no second dynamic-loader pass. Normalize their paths so an installed
        // worker under a home directory cannot recreate even an empty host directory hierarchy.
        let relative = if source == worker {
            PathBuf::from("worker")
        } else {
            Path::new("lib").join(source.file_name().ok_or_else(|| invalid("mount"))?)
        };
        let target = root.join(relative);
        fs::create_dir_all(target.parent().ok_or_else(|| invalid("mount"))?)
            .map_err(|e| error("mount", e))?;
        File::create(&target).map_err(|e| error("mount", e))?;
        mount(Some(&source), &target, None, libc::MS_BIND)?;
        mount(
            None,
            &target,
            None,
            libc::MS_BIND
                | libc::MS_REMOUNT
                | libc::MS_RDONLY
                | libc::MS_NOSUID
                | libc::MS_NODEV
                | locked_flags(&target)?,
        )?;
    }
    fs::create_dir(root.join("proc")).map_err(|e| error("mount", e))?;
    fs::create_dir(root.join("old")).map_err(|e| error("mount", e))?;
    std::env::set_current_dir(root).map_err(|e| error("mount", e))?;
    // SAFETY: both paths are valid C strings relative to our private tmpfs mount.
    check("mount", unsafe {
        libc::syscall(libc::SYS_pivot_root, c".".as_ptr(), c"old".as_ptr()) as i32
    })?;
    std::env::set_current_dir("/").map_err(|e| error("mount", e))?;
    // SAFETY: old root is a private mount and is detached before any untrusted code runs.
    check("mount", unsafe {
        libc::umount2(c"/old".as_ptr(), libc::MNT_DETACH)
    })?;
    fs::remove_dir("/old").map_err(|e| error("mount", e))?;
    mount(
        None,
        Path::new("/proc"),
        Some(c"proc"),
        libc::MS_RDONLY | libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
    )?;
    Ok(())
}
fn restrictions() -> Result<(), SandboxError> {
    for (resource, value) in [
        (libc::RLIMIT_NOFILE, 64),
        (libc::RLIMIT_CORE, 0),
        (libc::RLIMIT_FSIZE, 16 << 20),
    ] {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: valid rlimit structure; affects only this worker.
        check("limits", unsafe { libc::setrlimit(resource, &limit) })?;
    }
    // SAFETY: PR_SET_NO_NEW_PRIVS and capability queries operate on the current process only.
    check("capabilities", unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)
    })?;
    for cap in 0..64 {
        // SAFETY: PR_CAPBSET_READ takes an integer capability index.
        let present = unsafe { libc::prctl(libc::PR_CAPBSET_READ, cap, 0, 0, 0) };
        if present == -1 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::EINVAL) {
                break;
            }
            return Err(os("capabilities"));
        }
        // SAFETY: dropping a valid capability from this process's bounding set.
        check("capabilities", unsafe {
            libc::prctl(libc::PR_CAPBSET_DROP, cap, 0, 0, 0)
        })?;
    }
    #[repr(C)]
    struct Header {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Data {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    let header = Header {
        version: 0x20080522,
        pid: 0,
    };
    let data = [Data {
        effective: 0,
        permitted: 0,
        inheritable: 0,
    }; 2];
    // SAFETY: Linux capability ABI v3 uses this header and exactly two data words.
    check("capabilities", unsafe {
        libc::syscall(libc::SYS_capset, &header, data.as_ptr()) as i32
    })?;
    // SAFETY: close_range closes every descriptor except stdio and the bootstrap gate.
    check("fds", unsafe {
        libc::syscall(libc::SYS_close_range, 4u32, u32::MAX, 0u32) as i32
    })?;
    Ok(())
}

fn filter() -> Result<(), SandboxError> {
    use seccompiler::{
        BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
        SeccompRule,
    };
    let calls = [
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_clone3,
        libc::SYS_close,
        libc::SYS_fstat,
        libc::SYS_newfstatat,
        libc::SYS_statx, // Rust File metadata (including private /proc inspection).
        libc::SYS_lseek,
        libc::SYS_pread64,
        // Wasmtime uses sealed anonymous memfds for copy-on-write linear-memory images.
        libc::SYS_memfd_create,
        libc::SYS_ftruncate,
        libc::SYS_openat,
        libc::SYS_readlinkat,
        libc::SYS_getdents64,
        libc::SYS_fcntl,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_futex,
        libc::SYS_set_robust_list,
        libc::SYS_rseq,
        libc::SYS_set_tid_address,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_prlimit64,
        libc::SYS_getrlimit,
        libc::SYS_uname,
        libc::SYS_sysinfo,
        libc::SYS_exit,
        libc::SYS_exit_group,
        libc::SYS_tgkill,
        libc::SYS_prctl,
    ];
    let mut rules: std::collections::BTreeMap<_, _> =
        calls.into_iter().map(|n| (n, vec![])).collect();
    #[cfg(target_arch = "x86_64")]
    for n in [
        libc::SYS_arch_prctl,
        libc::SYS_readlink,
        libc::SYS_stat,
        libc::SYS_access,
    ] {
        rules.insert(n, vec![]);
    }
    // Threads only, no process clones or namespace flags. All other bits must match pthread's flags.
    let thread_flags = libc::CLONE_VM
        | libc::CLONE_FS
        | libc::CLONE_FILES
        | libc::CLONE_SIGHAND
        | libc::CLONE_THREAD
        | libc::CLONE_SYSVSEM
        | libc::CLONE_SETTLS
        | libc::CLONE_PARENT_SETTID
        | libc::CLONE_CHILD_CLEARTID;
    let condition = SeccompCondition::new(
        0,
        SeccompCmpArgLen::Qword,
        SeccompCmpOp::Eq,
        thread_flags as u64,
    )
    .map_err(|_| invalid("seccomp"))?;
    rules.insert(
        libc::SYS_clone,
        vec![SeccompRule::new(vec![condition]).map_err(|_| invalid("seccomp"))?],
    );
    #[cfg(target_arch = "aarch64")]
    {
        // Wasmtime's AArch64 JIT uses only private expedited instruction-stream barriers.
        // Older kernels requiring the global fallback are refused rather than affecting other jobs.
        let barriers = [32, 64]
            .into_iter()
            .map(|command| {
                let condition =
                    SeccompCondition::new(0, SeccompCmpArgLen::Dword, SeccompCmpOp::Eq, command)
                        .map_err(|_| invalid("seccomp"))?;
                SeccompRule::new(vec![condition]).map_err(|_| invalid("seccomp"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        rules.insert(libc::SYS_membarrier, barriers);
    }
    let arch = std::env::consts::ARCH
        .try_into()
        .map_err(|_| invalid("seccomp"))?;
    let program: BpfProgram = SeccompFilter::new(
        rules,
        SeccompAction::KillProcess,
        SeccompAction::Allow,
        arch,
    )
    .map_err(|_| invalid("seccomp"))?
    .try_into()
    .map_err(|_| invalid("seccomp"))?;
    // glibc tries clone3 before clone. Its pointer argument cannot be safely filtered by BPF;
    // ENOSYS forces the checked clone path, never permitting clone3 to execute.
    let fallback: BpfProgram = SeccompFilter::new(
        [(libc::SYS_clone3, vec![])].into_iter().collect(),
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        arch,
    )
    .map_err(|_| invalid("seccomp"))?
    .try_into()
    .map_err(|_| invalid("seccomp"))?;
    // clone3 is in the main allow-list so this stacked filter can return ENOSYS;
    // KILL would otherwise take precedence over ERRNO.
    seccompiler::apply_filter(&fallback).map_err(|_| os("seccomp"))?;
    seccompiler::apply_filter(&program).map_err(|_| os("seccomp"))?;
    Ok(())
}

/// Trusted entry point for the worker/probe, called before reading any job data.
/// Returns immediately for ordinary direct worker invocations (used by runtime tests).
#[doc(hidden)]
pub fn bootstrap() {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new(BOOTSTRAP)) {
        return;
    }
    let root = args.next().map(PathBuf::from);
    // SAFETY: fd 3 is the private stream inherited explicitly by spawn, owned once here as a File.
    let mut gate = unsafe { File::from_raw_fd(3) };
    let result = (|| {
        let root = root.ok_or_else(|| invalid("bootstrap"))?;
        let mut go = [0];
        gate.read_exact(&mut go)
            .map_err(|e| error("bootstrap", e))?;
        // SAFETY: UID/GID reads have no pointer arguments.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        // SAFETY: bootstrap is single-threaded; unshare affects only this trusted process.
        check("userns", unsafe { libc::unshare(libc::CLONE_NEWUSER) })?;
        fs::write("/proc/self/setgroups", "deny").map_err(|e| error("userns", e))?;
        fs::write("/proc/self/uid_map", format!("0 {uid} 1")).map_err(|e| error("userns", e))?;
        fs::write("/proc/self/gid_map", format!("0 {gid} 1")).map_err(|e| error("userns", e))?;
        // SAFETY: all namespaces are new and private; there are no application threads yet.
        check("namespaces", unsafe {
            libc::unshare(
                libc::CLONE_NEWNS
                    | libc::CLONE_NEWPID
                    | libc::CLONE_NEWNET
                    | libc::CLONE_NEWIPC
                    | libc::CLONE_NEWUTS
                    | libc::CLONE_NEWCGROUP,
            )
        })?;
        // SAFETY: our UTS namespace is private, and the constant hostname has seven bytes.
        check("namespaces", unsafe {
            libc::sethostname(c"nc-exec".as_ptr(), 7)
        })?;
        // SAFETY: bootstrap is a fresh single-threaded process, not the multithreaded executor.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            return Err(os("namespaces"));
        }
        if pid > 0 {
            // The trusted launcher keeps its gate copy until exit; it never reads job data.
            let mut status = 0;
            // SAFETY: pid is our own child; status is a valid out-pointer. Retry EINTR.
            while unsafe { libc::waitpid(pid, &mut status, 0) } == -1 {
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    std::process::exit(125);
                }
            }
            std::process::exit(if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            });
        }
        filesystem(&root)?;
        restrictions()?;
        filter()?;
        gate.write_all(&[0; 8]).map_err(|e| error("bootstrap", e))?;
        gate.read_exact(&mut go)
            .map_err(|e| error("bootstrap", e))?;
        Ok::<_, SandboxError>(())
    })();
    if let Err(e) = result {
        let mut reply = [0; 8];
        reply[0] = match e.step {
            "userns" => 1,
            "namespaces" => 2,
            "mount" => 3,
            "capabilities" => 4,
            "seccomp" => 5,
            _ => 6,
        };
        // Keep the wire diagnostic to a fixed OS errno; no paths or job data cross this channel.
        let errno = e
            .detail
            .rsplit_once("(os error ")
            .and_then(|(_, tail)| tail.strip_suffix(')'))
            .and_then(|n| n.parse::<i32>().ok())
            .unwrap_or(libc::EIO);
        reply[4..].copy_from_slice(&errno.to_ne_bytes());
        let _ = gate.write_all(&reply);
        std::process::exit(125);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostics_contain_only_os_error_text() {
        let context = std::io::Error::other("private wrapper context");
        assert_eq!(
            error("mount", context).detail,
            std::io::Error::from_raw_os_error(libc::EIO).to_string()
        );
        assert_eq!(
            error("userns", std::io::Error::from_raw_os_error(libc::EPERM)).detail,
            std::io::Error::from_raw_os_error(libc::EPERM).to_string()
        );
    }
    #[test]
    fn required_layers_refuse_before_starting_worker() {
        for step in ["cgroup", "userns"] {
            REFUSE.with(|r| r.set(Some(step)));
            let result = SandboxedChild::spawn(&SandboxSpec {
                worker: "/no-worker-may-run".into(),
                process_memory_bytes: 64 << 20,
            });
            REFUSE.with(|r| r.set(None));
            match result {
                Err(e) => assert_eq!(e.step, step),
                Ok(_) => panic!("unavailable layer ran a worker"),
            }
        }
    }
}
