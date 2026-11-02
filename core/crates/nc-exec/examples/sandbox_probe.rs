//! Test-only probe started **inside** the sandbox by `tests/sandbox_windows.rs` in place of the worker.
//! It plays an escaped job: reads commands from stdin (one per line) and prints one result line each.
//! Never shipped; examples are only built for tests.

use std::io::{BufRead, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

fn main() {
    #[cfg(target_os = "linux")]
    // SAFETY: this trusted fixture captures the executor's host PID before namespace setup so the
    // later escape probe can attempt ptrace against the actual parent, not its own namespace PID.
    let parent = unsafe { libc::getppid() };
    #[cfg(target_os = "linux")]
    nc_exec::sandbox::bootstrap();
    let mut stdin = std::io::stdin().lock();
    let mut out = std::io::stdout();
    // Given a real worker request, act as a hung worker that never replies (watchdog test).
    let mut prefix = [0u8; 4];
    if stdin.read_exact(&mut prefix).is_err() {
        return;
    }
    if &prefix == b"NCX1" {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    for line in std::io::BufReader::new((&prefix[..]).chain(stdin)).lines() {
        let Ok(line) = line else { break };
        let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        let res = match cmd {
            #[cfg(target_os = "linux")]
            "isolation" => {
                if linux_isolation() {
                    "ISOLATION_OK".into()
                } else {
                    "ISOLATION_FAILED".into()
                }
            }
            "hang" => loop {
                std::thread::sleep(Duration::from_secs(1));
            },
            "alloc" => {
                let mb: usize = arg.parse().unwrap_or(0);
                let mut v: Vec<Vec<u8>> = Vec::new();
                for _ in 0..mb {
                    v.push(vec![1u8; 1024 * 1024]);
                }
                format!("ALLOC_OK {}", v.len())
            }
            #[cfg(target_os = "linux")]
            "ptrace" | "unshare" | "mount" | "forkbomb" | "exec" => {
                // SAFETY: test-only escape probes use valid null/constant syscall arguments;
                // the sandbox must kill this process before any operation takes effect.
                unsafe {
                    match cmd {
                        "exec" => {
                            libc::execve(
                                c"/bin/sh".as_ptr(),
                                [c"sh".as_ptr(), std::ptr::null()].as_ptr(),
                                [std::ptr::null::<libc::c_char>()].as_ptr(),
                            );
                        }
                        "ptrace" => {
                            libc::ptrace(
                                libc::PTRACE_ATTACH,
                                parent,
                                std::ptr::null_mut::<libc::c_void>(),
                                std::ptr::null_mut::<libc::c_void>(),
                            );
                        }
                        "unshare" => {
                            libc::unshare(libc::CLONE_NEWUSER);
                        }
                        "mount" => {
                            libc::mount(
                                std::ptr::null(),
                                c"/".as_ptr(),
                                std::ptr::null(),
                                libc::MS_REMOUNT,
                                std::ptr::null(),
                            );
                        }
                        _ => {
                            libc::syscall(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0);
                        }
                    }
                }
                "ESCAPE_UNEXPECTED".into()
            }
            #[cfg(target_os = "linux")]
            "clone3" => {
                let args = [0u64; 11];
                // SAFETY: clone_args is an initialized, correctly sized buffer. The filter must return
                // ENOSYS without creating a child; immediately exit any unexpected child.
                let result = unsafe {
                    libc::syscall(
                        libc::SYS_clone3,
                        args.as_ptr(),
                        std::mem::size_of_val(&args),
                    )
                };
                if result == 0 {
                    std::process::exit(99);
                }
                format!(
                    "CLONE3_DENIED {}",
                    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
                )
            }
            #[cfg(target_os = "linux")]
            "threads" => {
                let mut handles = Vec::new();
                for _ in 0..128 {
                    match std::thread::Builder::new()
                        .stack_size(64 << 10)
                        .spawn(|| std::thread::sleep(Duration::from_millis(500)))
                    {
                        Ok(h) => handles.push(h),
                        Err(_) => break,
                    }
                }
                let count = handles.len();
                for h in handles {
                    let _ = h.join();
                }
                format!("THREADS_DENIED {count}")
            }
            "spawn" => {
                #[cfg(windows)]
                let shell = r"C:\Windows\System32\cmd.exe";
                #[cfg(not(windows))]
                let shell = "/bin/sh";
                match std::process::Command::new(shell)
                    .args(["/c", "exit 0"])
                    .status()
                {
                    Ok(_) => "SPAWN_OK".into(),
                    Err(e) => format!("SPAWN_DENIED {}", e.raw_os_error().unwrap_or(0)),
                }
            }
            #[cfg(target_os = "linux")]
            "dir" => match std::fs::read_dir(arg) {
                Ok(_) => "DIR_OK".into(),
                Err(e) => format!("DIR_DENIED {}", e.raw_os_error().unwrap_or(0)),
            },
            "read" => match std::fs::read(arg) {
                Ok(b) => format!("READ_OK {}", b.len()),
                Err(e) => format!("READ_DENIED {}", e.raw_os_error().unwrap_or(0)),
            },
            "write" => match std::fs::write(arg, b"x") {
                Ok(()) => {
                    let _ = std::fs::remove_file(arg);
                    "WRITE_OK".into()
                }
                Err(e) => format!("WRITE_DENIED {}", e.raw_os_error().unwrap_or(0)),
            },
            "tcp" => match arg.parse() {
                Ok(addr) => match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
                    Ok(_) => "TCP_OK".into(),
                    Err(e) => format!("TCP_DENIED {}", e.raw_os_error().unwrap_or(0)),
                },
                Err(_) => "TCP_BADADDR".into(),
            },
            "dns" => match (arg, 443).to_socket_addrs() {
                Ok(a) => {
                    if a.count() > 0 {
                        "DNS_OK".into()
                    } else {
                        "DNS_EMPTY".into()
                    }
                }
                Err(e) => format!("DNS_DENIED {}", e.raw_os_error().unwrap_or(0)),
            },
            "env" => format!("ENV_COUNT {}", std::env::vars_os().count()),
            _ => "UNKNOWN".into(),
        };
        let _ = writeln!(out, "{res}");
        let _ = out.flush();
    }
}

#[cfg(target_os = "linux")]
fn linux_isolation() -> bool {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    let capabilities = ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"]
        .iter()
        .all(|key| {
            status
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .is_some_and(|value| value.trim() == "0000000000000000")
        });
    let hardened =
        ["NoNewPrivs:", "Seccomp:"]
            .into_iter()
            .zip(["1", "2"])
            .all(|(key, expected)| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix(key))
                    .is_some_and(|value| value.trim() == expected)
            });
    let limits = [
        (libc::RLIMIT_NOFILE, 64),
        (libc::RLIMIT_CORE, 0),
        (libc::RLIMIT_FSIZE, 16 << 20),
    ]
    .into_iter()
    .all(|(resource, value)| {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: limit is a valid out-pointer to a properly sized rlimit structure.
        unsafe {
            libc::getrlimit(resource, &mut limit) == 0
                && limit.rlim_cur == value
                && limit.rlim_max == value
        }
    });
    let Ok(network) = std::fs::read_to_string("/proc/net/dev") else {
        return false;
    };
    let interfaces: Vec<_> = network
        .lines()
        .filter_map(|line| line.split_once(':').map(|(name, _)| name.trim()))
        .collect();
    let Ok(descriptors) = std::fs::read_dir("/proc/self/fd") else {
        return false;
    };
    let names: Vec<_> = descriptors
        .filter_map(Result::ok)
        .map(|entry| entry.file_name())
        .collect();
    // read_dir itself temporarily holds fd 3; the bootstrap control descriptor is already closed.
    let fds = names.len() == 4
        && (0..4).all(|fd| {
            names
                .iter()
                .any(|n| n == std::ffi::OsStr::new(&fd.to_string()))
        });
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").is_ok_and(|s| s.trim() == "0::/");
    // Regression: procfs must belong to this PID namespace after pivot_root, with no
    // inherited host mounts. Verify kernel state without printing paths or mount data.
    let private_proc = std::fs::read_dir("/proc").is_ok_and(|entries| {
        let pids: Vec<_> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
            .collect();
        pids == [1]
    });
    let proc_mount = std::fs::read_to_string("/proc/self/mountinfo").is_ok_and(|mounts| {
        mounts.lines().any(|line| {
            let Some((entry, filesystem)) = line.split_once(" - ") else {
                return false;
            };
            let fields: Vec<_> = entry.split_whitespace().collect();
            fields.get(4) == Some(&"/proc")
                && filesystem.starts_with("proc ")
                && fields.get(5).is_some_and(|flags| {
                    ["ro", "nosuid", "nodev", "noexec"]
                        .iter()
                        .all(|required| flags.split(',').any(|flag| flag == *required))
                })
        })
    });
    let minimal_root = std::fs::read_dir("/").is_ok_and(|entries| {
        entries.filter_map(Result::ok).all(|entry| {
            ["worker", "lib", "proc"]
                .iter()
                .any(|name| entry.file_name() == std::ffi::OsStr::new(name))
        })
    });
    private_proc
        && proc_mount
        && minimal_root
        && capabilities
        && hardened
        && limits
        && interfaces == ["lo"]
        && fds
        && cgroup
        && std::process::id() == 1
        && std::env::vars_os().count() == 0
}
