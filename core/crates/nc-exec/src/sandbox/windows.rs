//! Windows sandbox: fresh no-capability AppContainer + Job Object per job (recipe proven in the legacy
//! spike `legacy/python-v0.3:spikes/0.3-sandbox/REPORT.md`, ported to Rust).
//!
//! Launch: `CreateAppContainerProfile(caps = none)` → Job Object (process + job memory cap,
//! `ActiveProcessLimit = 1`, kill-on-close, die-on-unhandled-exception, all UI limits, completion port) →
//! `CreateProcessW` with `STARTUPINFOEX` attributes `SECURITY_CAPABILITIES`, `HANDLE_LIST` (3 pipe ends only),
//! `JOB_LIST` (no suspend/assign/resume race) and `CHILD_PROCESS_POLICY = restricted`; `DETACHED_PROCESS` so no
//! `conhost.exe` joins the job; minimal environment; cwd = the container's own folder.
//! Teardown: `TerminateJobObject` → wait → close handles (kill-on-close) → `DeleteAppContainerProfile`.

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::{FromRawHandle, RawHandle};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT, HLOCAL,
    STILL_ACTIVE, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, GetAppContainerFolderPath,
};
use windows::Win32::Security::{FreeSid, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JobObjectAssociateCompletionPortInformation, JobObjectBasicUIRestrictions,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_UI_RESTRICTIONS,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION, JOB_OBJECT_LIMIT_JOB_MEMORY,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOB_OBJECT_UILIMIT,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcess, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, BELOW_NORMAL_PRIORITY_CLASS, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_JOB_LIST, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};
use windows::Win32::System::IO::{GetQueuedCompletionStatus, OVERLAPPED};

use super::{ChildPipes, SandboxError, SandboxEvent, SandboxSpec, SetupTimings};

/// Every profile this crate creates starts with this (Windows stores names in lower case).
/// Full name: `nc-exec-<owner pid>-<nonce>`, so the sweep can tell whose leftovers they are.
pub const PROFILE_PREFIX: &str = "nc-exec-";

// Values from winnt.h (not exported by this windows feature set).
const JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT: u32 = 3;
const JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT: u32 = 9;
const JOB_OBJECT_MSG_JOB_MEMORY_LIMIT: u32 = 10;
const JOB_OBJECT_UILIMIT_ALL: u32 = 0x1FF;
const PROCESS_CREATION_CHILD_PROCESS_RESTRICTED: u32 = 1;

/// Exit code used when we kill the job.
const KILLED: u32 = 0xE000_0001;
const MAPPINGS: &str = r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings";
/// SID string of `ALL APPLICATION PACKAGES`.
const ALL_APP_PACKAGES: &str = "*S-1-15-2-1";

static NONCE: AtomicU64 = AtomicU64::new(0);

fn err(step: &'static str, e: impl std::fmt::Display) -> SandboxError {
    SandboxError {
        step,
        detail: e.to_string(),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Owned kernel handle, closed on drop.
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: we own this handle (returned by a successful Create* call) and close it exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

impl Owned {
    /// Give up ownership, turning the handle into a `std::fs::File`.
    fn into_file(self) -> File {
        let raw = self.0 .0 as RawHandle;
        std::mem::forget(self);
        // SAFETY: `raw` is an open pipe handle we own; ownership moves into the File, which closes it once.
        unsafe { File::from_raw_handle(raw) }
    }
}

/// Inheritable pipe: returns (read end, write end). `child_reads` decides which end the child gets;
/// the parent's end is made non-inheritable.
fn pipe(child_reads: bool) -> Result<(Owned, Owned), SandboxError> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let mut r = HANDLE::default();
    let mut w = HANDLE::default();
    // SAFETY: out-pointers are valid locals; `sa` lives across the call.
    unsafe { CreatePipe(&mut r, &mut w, Some(&sa), 0) }.map_err(|e| err("CreatePipe", e))?;
    let (r, w) = (Owned(r), Owned(w));
    let parent = if child_reads { &w } else { &r };
    // SAFETY: `parent.0` is a valid handle we own.
    unsafe { SetHandleInformation(parent.0, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0)) }
        .map_err(|e| err("SetHandleInformation", e))?;
    Ok((r, w))
}

/// Proc-thread attribute list buffer with RAII delete.
struct AttrList {
    buf: Vec<u8>,
    initialised: bool,
}

impl AttrList {
    fn new(count: u32) -> Result<Self, SandboxError> {
        let mut size = 0usize;
        // SAFETY: size query; Windows only writes `size`. The call "fails" with INSUFFICIENT_BUFFER by design.
        let _ = unsafe { InitializeProcThreadAttributeList(None, count, None, &mut size) };
        let mut buf = vec![0u8; size];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
        // SAFETY: `buf` has the size Windows asked for and outlives the list (deleted in Drop).
        unsafe { InitializeProcThreadAttributeList(Some(list), count, None, &mut size) }
            .map_err(|e| err("InitializeProcThreadAttributeList", e))?;
        Ok(Self {
            buf,
            initialised: true,
        })
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buf.as_mut_ptr().cast())
    }

    /// # Safety
    /// `value` must point to `size` readable bytes that stay alive until the process is created.
    unsafe fn set(
        &mut self,
        attr: u32,
        value: *const c_void,
        size: usize,
        step: &'static str,
    ) -> Result<(), SandboxError> {
        let list = self.ptr();
        // SAFETY: caller guarantees `value`/`size`; `list` is initialised.
        unsafe { UpdateProcThreadAttribute(list, 0, attr as usize, Some(value), size, None, None) }
            .map_err(|e| err(step, e))
    }
}

impl Drop for AttrList {
    fn drop(&mut self) {
        if self.initialised {
            let list = self.ptr();
            // SAFETY: the list was initialised by InitializeProcThreadAttributeList and is deleted once.
            unsafe { DeleteProcThreadAttributeList(list) };
        }
    }
}

/// AppContainer profile; deleted on drop.
struct Profile {
    name: String,
    sid: PSID,
    folder: String,
}

impl Profile {
    fn create() -> Result<Self, SandboxError> {
        let name = format!(
            "{PROFILE_PREFIX}{}-{:x}{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let h = HSTRING::from(name.as_str());
        let desc = HSTRING::from("Network Connector job sandbox");
        // SAFETY: strings are valid NUL-terminated wide strings for the call; no capabilities passed.
        let sid = unsafe { CreateAppContainerProfile(&h, &h, &desc, None) }
            .map_err(|e| err("CreateAppContainerProfile", e))?;
        let mut p = Self {
            name,
            sid,
            folder: String::new(),
        };
        p.folder = p.folder_path()?;
        Ok(p)
    }

    fn folder_path(&self) -> Result<String, SandboxError> {
        let mut s = PWSTR::null();
        // SAFETY: `self.sid` is a valid SID from CreateAppContainerProfile; `s` is a valid out-pointer.
        unsafe { ConvertSidToStringSidW(self.sid, &mut s) }
            .map_err(|e| err("ConvertSidToStringSidW", e))?;
        // SAFETY: `s` is a NUL-terminated string allocated by Windows.
        let sid_str = unsafe { s.to_string() }.unwrap_or_default();
        // SAFETY: `s` was allocated with LocalAlloc by ConvertSidToStringSidW and is freed once.
        unsafe {
            let _ = windows::Win32::Foundation::LocalFree(Some(HLOCAL(s.0.cast())));
        }
        let hs = HSTRING::from(sid_str.as_str());
        // SAFETY: `hs` is a valid wide string.
        let p = unsafe { GetAppContainerFolderPath(&hs) }
            .map_err(|e| err("GetAppContainerFolderPath", e))?;
        // SAFETY: `p` is a NUL-terminated string allocated by Windows with CoTaskMemAlloc.
        let folder = unsafe { p.to_string() }.unwrap_or_default();
        // SAFETY: freed once, with the allocator that produced it.
        unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
        Ok(folder)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        // SAFETY: `sid` came from CreateAppContainerProfile and is freed once.
        unsafe {
            let _ = FreeSid(self.sid);
        }
        let h = HSTRING::from(self.name.as_str());
        // SAFETY: valid wide string.
        let _ = unsafe { DeleteAppContainerProfile(&h) };
    }
}

fn make_job(spec: &SandboxSpec) -> Result<(Owned, Owned), SandboxError> {
    // SAFETY: anonymous job, default security.
    let job = Owned(
        unsafe { CreateJobObjectW(None, PCWSTR::null()) }.map_err(|e| err("CreateJobObject", e))?,
    );
    let mem = usize::try_from(spec.process_memory_bytes).map_err(|e| err("memory cap", e))?;
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT(
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.0
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION.0
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS.0
            | JOB_OBJECT_LIMIT_PROCESS_MEMORY.0
            | JOB_OBJECT_LIMIT_JOB_MEMORY.0,
    );
    info.BasicLimitInformation.ActiveProcessLimit = 1;
    info.ProcessMemoryLimit = mem;
    info.JobMemoryLimit = mem;
    // SAFETY: `info` is the struct matching this info class, with its exact size.
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    }
    .map_err(|e| err("SetInformationJobObject(limits)", e))?;
    let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
        UIRestrictionsClass: JOB_OBJECT_UILIMIT(JOB_OBJECT_UILIMIT_ALL),
    };
    // SAFETY: matching struct and size.
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectBasicUIRestrictions,
            (&ui as *const JOBOBJECT_BASIC_UI_RESTRICTIONS).cast(),
            size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
        )
    }
    .map_err(|e| err("SetInformationJobObject(ui)", e))?;
    // SAFETY: creating a fresh completion port not tied to a file (INVALID_HANDLE_VALUE).
    let port = Owned(
        unsafe {
            windows::Win32::System::IO::CreateIoCompletionPort(
                windows::Win32::Foundation::INVALID_HANDLE_VALUE,
                None,
                0,
                1,
            )
        }
        .map_err(|e| err("CreateIoCompletionPort", e))?,
    );
    let assoc = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
        CompletionKey: std::ptr::null_mut(),
        CompletionPort: port.0,
    };
    // SAFETY: matching struct and size; the port outlives the job (both owned by SandboxedChild, job closed first).
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectAssociateCompletionPortInformation,
            (&assoc as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
            size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
        )
    }
    .map_err(|e| err("SetInformationJobObject(port)", e))?;
    Ok((job, port))
}

fn env_block(folder: &str) -> Vec<u16> {
    let mut s = String::new();
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    // LOCALAPPDATA must be present or CreateProcess fails with ERROR_ENVVAR_NOT_FOUND for AppContainers;
    // Windows then redirects LOCALAPPDATA/TEMP/TMP into the container folder.
    let lad = std::env::var("LOCALAPPDATA").unwrap_or_default();
    for (k, v) in [
        ("SystemRoot", root.as_str()),
        ("LOCALAPPDATA", lad.as_str()),
        ("TEMP", folder),
        ("TMP", folder),
    ] {
        s.push_str(k);
        s.push('=');
        s.push_str(v);
        s.push('\0');
    }
    s.push('\0');
    s.encode_utf16().collect()
}

/// A running worker inside AppContainer + Job Object. Dropping it tears everything down.
pub struct SandboxedChild {
    process: Owned,
    thread: Owned,
    job: Option<Owned>,
    port: Owned,
    profile: Option<Profile>,
    timings: SetupTimings,
    torn_down: bool,
}

impl std::fmt::Debug for SandboxedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SandboxedChild").finish_non_exhaustive()
    }
}

impl SandboxedChild {
    pub(super) fn spawn(spec: &SandboxSpec) -> Result<(Self, ChildPipes), SandboxError> {
        let t0 = Instant::now();
        let profile = Profile::create()?;
        let t_profile = t0.elapsed();
        let t1 = Instant::now();
        let (job, port) = make_job(spec)?;

        let (in_r, in_w) = pipe(true)?;
        let (out_r, out_w) = pipe(false)?;
        let (err_r, err_w) = pipe(false)?;

        let mut attrs = AttrList::new(4)?;
        let caps = SECURITY_CAPABILITIES {
            AppContainerSid: profile.sid,
            Capabilities: std::ptr::null_mut(),
            CapabilityCount: 0,
            Reserved: 0,
        };
        let handles: [HANDLE; 3] = [in_r.0, out_w.0, err_w.0];
        let jobs: [HANDLE; 1] = [job.0];
        let child_policy: u32 = PROCESS_CREATION_CHILD_PROCESS_RESTRICTED;
        // SAFETY: each value is a local that lives until after CreateProcessW below; sizes are exact.
        unsafe {
            attrs.set(
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES,
                (&caps as *const SECURITY_CAPABILITIES).cast(),
                size_of::<SECURITY_CAPABILITIES>(),
                "attr SECURITY_CAPABILITIES",
            )?;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                handles.as_ptr().cast(),
                size_of_val(&handles),
                "attr HANDLE_LIST",
            )?;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_JOB_LIST,
                jobs.as_ptr().cast(),
                size_of_val(&jobs),
                "attr JOB_LIST",
            )?;
            attrs.set(
                PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
                (&child_policy as *const u32).cast(),
                size_of::<u32>(),
                "attr CHILD_PROCESS_POLICY",
            )?;
        }

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = in_r.0;
        si.StartupInfo.hStdOutput = out_w.0;
        si.StartupInfo.hStdError = err_w.0;
        si.lpAttributeList = attrs.ptr();

        let exe = spec.worker.to_string_lossy().into_owned();
        let app = wide(&exe);
        let mut cmdline = wide(&format!("\"{exe}\""));
        let cwd = wide(&profile.folder);
        let env = env_block(&profile.folder);
        let mut pi = PROCESS_INFORMATION::default();
        // SAFETY: all pointers reference live locals (app, cmdline, env, cwd, si, attrs and the attribute
        // values above); `cmdline` is mutable as CreateProcessW requires.
        unsafe {
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | DETACHED_PROCESS
                    | CREATE_SUSPENDED
                    | BELOW_NORMAL_PRIORITY_CLASS,
                Some(env.as_ptr().cast()),
                PCWSTR(cwd.as_ptr()),
                &si.StartupInfo,
                &mut pi,
            )
        }
        .map_err(|e| err("CreateProcessW", e))?;
        drop(attrs);
        // Close the child's ends in the parent so the pipes reach EOF when the child dies.
        drop((in_r, out_w, err_w));
        let child = Self {
            process: Owned(pi.hProcess),
            thread: Owned(pi.hThread),
            job: Some(job),
            port,
            profile: Some(profile),
            timings: SetupTimings {
                profile: t_profile,
                launch: t1.elapsed(),
            },
            torn_down: false,
        };
        let pipes = ChildPipes {
            stdin: in_w.into_file(),
            stdout: out_r.into_file(),
            stderr: err_r.into_file(),
        };
        Ok((child, pipes))
    }

    /// Let the (suspended) worker start running.
    pub fn resume(&mut self) -> Result<(), SandboxError> {
        // SAFETY: valid thread handle owned by self.
        if unsafe { ResumeThread(self.thread.0) } == u32::MAX {
            return Err(err("ResumeThread", windows::core::Error::from_thread()));
        }
        Ok(())
    }

    /// Wait up to `timeout` for the worker to exit; returns its exit code if it has.
    pub fn wait(&mut self, timeout: Duration) -> Option<u32> {
        let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: valid process handle owned by self.
        if unsafe { WaitForSingleObject(self.process.0, ms) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: valid process handle, valid out-pointer.
        unsafe { GetExitCodeProcess(self.process.0, &mut code) }.ok()?;
        Some(code)
    }

    /// Drain Job Object notifications (non-blocking).
    pub fn events(&mut self) -> Vec<SandboxEvent> {
        let mut out = Vec::new();
        loop {
            let mut msg = 0u32;
            let mut key = 0usize;
            let mut ov: *mut OVERLAPPED = std::ptr::null_mut();
            // SAFETY: valid port handle; out-pointers are locals; timeout 0 = poll.
            if unsafe { GetQueuedCompletionStatus(self.port.0, &mut msg, &mut key, &mut ov, 0) }
                .is_err()
            {
                break;
            }
            match msg {
                JOB_OBJECT_MSG_PROCESS_MEMORY_LIMIT | JOB_OBJECT_MSG_JOB_MEMORY_LIMIT => {
                    out.push(SandboxEvent::MemoryLimit)
                }
                JOB_OBJECT_MSG_ACTIVE_PROCESS_LIMIT => out.push(SandboxEvent::ProcessLimit),
                _ => {}
            }
        }
        out
    }

    /// Kill everything in the job now.
    pub fn kill(&mut self) {
        if let Some(job) = &self.job {
            // SAFETY: valid job handle owned by self.
            let _ = unsafe { TerminateJobObject(job.0, KILLED) };
        }
    }

    /// Setup timings.
    pub fn timings(&self) -> SetupTimings {
        self.timings
    }

    /// Peak committed memory of the worker, from the Job Object.
    pub fn peak_memory_bytes(&self) -> u64 {
        let Some(job) = &self.job else { return 0 };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        // SAFETY: matching struct and size; valid job handle.
        let ok = unsafe {
            QueryInformationJobObject(
                Some(job.0),
                JobObjectExtendedLimitInformation,
                (&mut info as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                None,
            )
        };
        if ok.is_ok() {
            info.PeakProcessMemoryUsed as u64
        } else {
            0
        }
    }

    /// AppContainer folder of this sandbox (for tests that check it is removed).
    pub fn container_folder(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.folder.as_str())
    }

    /// Kill, wait, close the job (kill-on-close) and delete the profile. Returns time taken.
    pub fn teardown(mut self) -> Duration {
        self.teardown_inner()
    }

    fn teardown_inner(&mut self) -> Duration {
        let t = Instant::now();
        if self.torn_down {
            return Duration::ZERO;
        }
        self.torn_down = true;
        self.kill();
        let _ = self.wait(Duration::from_secs(2));
        self.job = None; // CloseHandle → KILL_ON_JOB_CLOSE
        self.profile = None; // FreeSid + DeleteAppContainerProfile
        t.elapsed()
    }
}

impl Drop for SandboxedChild {
    fn drop(&mut self) {
        self.teardown_inner();
        let _ = (&self.thread, &self.port);
    }
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: querying another process; the handle is closed by `Owned`.
    match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(h) => {
            let h = Owned(h);
            let mut code = 0u32;
            // SAFETY: valid handle, valid out-pointer.
            unsafe { GetExitCodeProcess(h.0, &mut code) }.is_ok() && code == STILL_ACTIVE.0 as u32
        }
        Err(_) => false,
    }
}

/// List the names of all AppContainer profiles of this user whose name starts with [`PROFILE_PREFIX`].
pub fn list_profiles() -> Result<Vec<String>, SandboxError> {
    let mut key = HKEY::default();
    let sub = HSTRING::from(MAPPINGS);
    // SAFETY: valid key, string and out-pointer.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, &sub, None, KEY_READ, &mut key) };
    if rc.is_err() {
        // No AppContainer has ever been created for this user.
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    let mut i = 0u32;
    loop {
        let mut buf = [0u16; 256];
        let mut len = buf.len() as u32;
        // SAFETY: `buf`/`len` describe a valid writable buffer.
        let rc = unsafe {
            RegEnumKeyExW(
                key,
                i,
                Some(PWSTR(buf.as_mut_ptr())),
                &mut len,
                None,
                None,
                None,
                None,
            )
        };
        if rc.is_err() {
            break;
        }
        i += 1;
        let sid = String::from_utf16_lossy(&buf[..len as usize]);
        let mut val = [0u16; 512];
        let mut cb = (val.len() * 2) as u32;
        let sub = HSTRING::from(sid.as_str());
        let vname = HSTRING::from("Moniker");
        // SAFETY: valid key/strings; `val`/`cb` describe a writable buffer of `cb` bytes.
        let rc = unsafe {
            RegGetValueW(
                key,
                &sub,
                &vname,
                RRF_RT_REG_SZ,
                None,
                Some(val.as_mut_ptr().cast()),
                Some(&mut cb),
            )
        };
        if rc.is_err() {
            continue;
        }
        let n = (cb as usize / 2).saturating_sub(1);
        let moniker = String::from_utf16_lossy(&val[..n.min(val.len())]);
        if moniker.to_ascii_lowercase().starts_with(PROFILE_PREFIX) {
            names.push(moniker);
        }
    }
    // SAFETY: key opened above, closed once.
    let _ = unsafe { RegCloseKey(key) };
    Ok(names)
}

/// Delete leftover `nc-exec-*` profiles (and with them their folders) whose owning process is gone.
/// Profiles of live executors (including this process) are left alone. Returns how many were removed.
pub fn sweep_leftovers() -> Result<usize, SandboxError> {
    let me = std::process::id();
    let mut removed = 0;
    for name in list_profiles()? {
        let owner = name[PROFILE_PREFIX.len()..]
            .split('-')
            .next()
            .and_then(|p| p.parse::<u32>().ok());
        let stale = match owner {
            Some(pid) => pid != me && !process_alive(pid),
            None => true,
        };
        if stale {
            let h = HSTRING::from(name.as_str());
            // SAFETY: valid wide string.
            if unsafe { DeleteAppContainerProfile(&h) }.is_ok() {
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// Create a profile with an arbitrary name (tests simulate crash leftovers with it). Returns the folder.
#[doc(hidden)]
pub fn create_named_profile_for_test(name: &str) -> Result<String, SandboxError> {
    let h = HSTRING::from(name);
    // SAFETY: valid wide strings, no capabilities.
    let sid = unsafe { CreateAppContainerProfile(&h, &h, &h, None) }
        .map_err(|e| err("CreateAppContainerProfile", e))?;
    let p = Profile {
        name: name.to_string(),
        sid,
        folder: String::new(),
    };
    let folder = p.folder_path();
    // Keep the profile on disk: free the SID only, do not delete.
    let p = std::mem::ManuallyDrop::new(p);
    // SAFETY: SID from CreateAppContainerProfile, freed once; the Profile is never dropped.
    unsafe {
        let _ = FreeSid(p.sid);
    }
    folder
}

/// Grant `ALL APPLICATION PACKAGES` read+execute on `dir` so the worker binary can start inside an
/// AppContainer. Only the given folder is changed (no parents, no system ACLs). Uses `icacls` from System32.
pub fn grant_appcontainer_read(dir: &Path) -> Result<(), SandboxError> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let icacls = Path::new(&root).join("System32").join("icacls.exe");
    let out = std::process::Command::new(icacls)
        .arg(dir)
        .arg("/grant")
        .arg(format!("{ALL_APP_PACKAGES}:(OI)(CI)(RX)"))
        .output()
        .map_err(|e| err("icacls", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(err("icacls", format!("exit {:?}", out.status.code())))
    }
}
