//! Test-only probe started **inside** the sandbox by `tests/sandbox_windows.rs` in place of the worker.
//! It plays an escaped job: reads commands from stdin (one per line) and prints one result line each.
//! Never shipped; examples are only built for tests.

use std::io::{BufRead, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

fn main() {
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
            "spawn" => {
                match std::process::Command::new(r"C:\Windows\System32\cmd.exe")
                    .args(["/c", "exit 0"])
                    .status()
                {
                    Ok(_) => "SPAWN_OK".into(),
                    Err(e) => format!("SPAWN_DENIED {}", e.raw_os_error().unwrap_or(0)),
                }
            }
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
