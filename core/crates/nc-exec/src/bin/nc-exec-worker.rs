//! `nc-exec-worker`: single-use executor process. Started only by the `nc-exec` library inside the OS sandbox.
//!
//! Reads one framed [`nc_exec::ipc::WorkerRequest`] from stdin, runs it, writes one
//! [`nc_exec::ipc::WorkerResponse`] to stdout and exits. It opens no files, sockets or environment of its own.

use std::io::{self, BufReader, BufWriter, Write};
use std::process::ExitCode;

use nc_exec::ipc::WorkerRequest;
use nc_exec::runtime::execute_on_big_stack;

fn main() -> ExitCode {
    let req = match WorkerRequest::read_from(&mut BufReader::new(io::stdin().lock())) {
        Ok(r) => r,
        Err(_) => return ExitCode::from(2),
    };
    let resp = execute_on_big_stack(req);
    let mut out = BufWriter::new(io::stdout().lock());
    if resp.write_to(&mut out).is_err() || out.flush().is_err() {
        return ExitCode::from(3);
    }
    ExitCode::SUCCESS
}
