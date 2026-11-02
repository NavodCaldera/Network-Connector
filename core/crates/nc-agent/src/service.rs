//! Windows service host for nc-agent (SCM install/uninstall/start/stop and the service entry point).
//!
//! - Account: the per-service virtual account `NT SERVICE\nc-agent` (least privilege, not LocalSystem). It owns
//!   nothing on the machine except the data dir granted below.
//! - Data dir: must be under `%ProgramData%`; inheritance is cut and only SYSTEM, Administrators and the
//!   service account get access. The device key, control token, journal and logs live there.
//! - SCM Stop/Shutdown is turned into the existing authenticated loopback `Shutdown` control command, so the
//!   helper session stops exactly as `nc stop` + daemon exit does (job cancelled within 1 s, lease failed with
//!   `STOP`, sandbox torn down).
//! - Restart on failure: 5 s, then 30 s, then none; counter resets after a day; non-crash failures count.
//! - Logs: `<data_dir>\logs\nc-agent.log` (the same tracing events as the console; no secrets are logged).

use crate::{load, run_daemon};
use anyhow::{bail, Context, Result};
use nc_agent::config::{AgentConfig, ENROLL_ENV};
use nc_agent::control::{self, Command};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept,
    ServiceErrorControl, ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod,
    ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

/// SCM service name.
pub const SERVICE_NAME: &str = "nc-agent";
/// Least-privilege virtual service account.
pub const ACCOUNT: &str = r"NT SERVICE\nc-agent";
const DISPLAY_NAME: &str = "Network Connector helper";
const OWN: ServiceType = ServiceType::OWN_PROCESS;
const SCM_WAIT: Duration = Duration::from_secs(15);

static CONFIG: OnceLock<PathBuf> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

/// Entry point when the SCM starts `nc-agent service run --config <p>`.
pub fn dispatch(config: PathBuf) -> Result<()> {
    let _ = CONFIG.set(config);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("service dispatcher failed (only the SCM may start `service run`)")
}

fn service_main(_args: Vec<OsString>) {
    // Errors are reported to the SCM as a service-specific exit code (which triggers the restart policy).
    let _ = run_service();
}

fn status(state: ServiceState, code: u32, wait: Duration) -> ServiceStatus {
    ServiceStatus {
        service_type: OWN,
        current_state: state,
        controls_accepted: if state == ServiceState::Running {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: if code == 0 {
            ServiceExitCode::Win32(0)
        } else {
            ServiceExitCode::ServiceSpecific(code)
        },
        checkpoint: 0,
        wait_hint: wait,
        process_id: None,
    }
}

/// Where Stop sends the loopback Shutdown, once the daemon is up.
#[derive(Default)]
struct StopTarget {
    target: Option<(u16, String)>,
    requested: bool,
}

fn send_shutdown(port: u16, token: String) {
    // Off the SCM handler thread: the handler must return at once.
    std::thread::spawn(move || {
        if let Err(e) = control::call(port, &token, Command::Shutdown) {
            tracing::error!(error = %e, "service stop: loopback shutdown failed");
        }
    });
}

fn run_service() -> Result<()> {
    let stop = Arc::new(Mutex::new(StopTarget::default()));
    let st = Arc::clone(&stop);
    let handle = service_control_handler::register(SERVICE_NAME, move |c| match c {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let mut g = st.lock().unwrap_or_else(|p| p.into_inner());
            g.requested = true;
            if let Some((port, token)) = g.target.clone() {
                tracing::info!("service stop requested");
                send_shutdown(port, token);
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    })
    .context("registering the service control handler")?;
    handle.set_service_status(status(
        ServiceState::StartPending,
        0,
        Duration::from_secs(30),
    ))?;

    let result = (|| -> Result<()> {
        let path = CONFIG.get().context("no config path")?;
        let cfg = load(path)?;
        init_file_logs(&cfg)?;
        tracing::info!("nc-agent service starting");
        run_daemon(cfg, |port, token| {
            let _ = handle.set_service_status(status(ServiceState::Running, 0, Duration::ZERO));
            let mut g = stop.lock().unwrap_or_else(|p| p.into_inner());
            g.target = Some((port, token.to_string()));
            if g.requested {
                send_shutdown(port, token.to_string());
            }
        })
    })();
    let code = match &result {
        Ok(()) => 0,
        Err(e) => {
            tracing::error!(error = %e, "nc-agent service failed");
            1
        }
    };
    handle.set_service_status(status(ServiceState::Stopped, code, Duration::ZERO))?;
    result
}

fn init_file_logs(cfg: &AgentConfig) -> Result<()> {
    let dir = cfg.data_dir.join("logs");
    std::fs::create_dir_all(&dir).context("creating the log dir")?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("nc-agent.log"))
        .context("opening the log file")?;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(Mutex::new(file))
        .try_init()
        .map_err(|e| anyhow::anyhow!("log init: {e}"))
}

fn icacls(args: &[&OsStr]) -> Result<()> {
    let out = std::process::Command::new("icacls")
        .args(args)
        .output()
        .context("running icacls")?;
    if !out.status.success() {
        bail!(
            "icacls failed: {}",
            String::from_utf8_lossy(&out.stdout).trim()
        );
    }
    Ok(())
}

/// `data_dir` must be an absolute path under `%ProgramData%`.
fn check_data_dir(data_dir: &Path, program_data: &Path) -> Result<PathBuf> {
    let d = std::path::absolute(data_dir).context("data_dir")?;
    if !d.starts_with(program_data) || d == program_data {
        bail!(
            "data_dir must be a folder under {} for the service",
            program_data.display()
        );
    }
    Ok(d)
}

/// Install the service (elevated). Prepares the data dir ACL and, if `NC_ENROLL_SECRET` is set, stores it in
/// `enroll.secret` inside that dir.
pub fn install(config: &Path, cfg: &AgentConfig) -> Result<()> {
    let program_data = PathBuf::from(std::env::var_os("ProgramData").context("ProgramData unset")?);
    let data_dir = check_data_dir(&cfg.data_dir, &program_data)?;
    let config = std::path::absolute(config).context("config path")?;
    let exe = std::env::current_exe().context("current exe")?;

    let mgr = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("opening the service manager (run elevated)")?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: OWN,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: exe,
        launch_arguments: vec![
            OsString::from("service"),
            OsString::from("run"),
            OsString::from("--config"),
            config.clone().into_os_string(),
        ],
        dependencies: vec![],
        account_name: Some(OsString::from(ACCOUNT)),
        account_password: None,
    };
    let svc = mgr
        .create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)
        .context("creating the service")?;
    svc.set_description(
        "Runs sandboxed Wasm jobs for Network Connector within the owner's policy.",
    )?;
    let restart = |s| ServiceAction {
        action_type: ServiceActionType::Restart,
        delay: Duration::from_secs(s),
    };
    svc.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(86_400)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            restart(5),
            restart(30),
            ServiceAction {
                action_type: ServiceActionType::None,
                delay: Duration::ZERO,
            },
        ]),
    })?;
    svc.set_failure_actions_on_non_crash_failures(true)?;

    // The service SID exists now; lock the data dir down to SYSTEM, Administrators and the service.
    std::fs::create_dir_all(&data_dir).context("creating data_dir")?;
    let grant_svc = format!("{ACCOUNT}:(OI)(CI)M");
    icacls(&[
        data_dir.as_os_str(),
        OsStr::new("/inheritance:r"),
        OsStr::new("/grant:r"),
        OsStr::new("*S-1-5-18:(OI)(CI)F"),
        OsStr::new("*S-1-5-32-544:(OI)(CI)F"),
        OsStr::new(&grant_svc),
    ])?;
    if !config.starts_with(&data_dir) {
        let grant_cfg = format!("{ACCOUNT}:R");
        icacls(&[
            config.as_os_str(),
            OsStr::new("/grant"),
            OsStr::new(&grant_cfg),
        ])?;
    }
    if let Ok(secret) = std::env::var(ENROLL_ENV) {
        std::fs::write(cfg.enroll_secret_path(), secret.trim()).context("writing enroll.secret")?;
        println!("enrolment secret stored in the data dir (not printed)");
    }
    println!(
        "installed service {SERVICE_NAME} as {ACCOUNT}; data dir {}",
        data_dir.display()
    );
    Ok(())
}

fn wait_for(svc: &windows_service::service::Service, want: ServiceState) -> Result<Duration> {
    let t0 = Instant::now();
    while t0.elapsed() < SCM_WAIT {
        if svc.query_status()?.current_state == want {
            return Ok(t0.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("service did not reach {want:?} within {SCM_WAIT:?}")
}

fn open(access: ServiceAccess) -> Result<windows_service::service::Service> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("opening the service manager")?;
    mgr.open_service(SERVICE_NAME, access)
        .context("opening the nc-agent service (installed? elevated?)")
}

/// Start the service and wait until it runs.
pub fn start() -> Result<()> {
    let svc = open(ServiceAccess::START | ServiceAccess::QUERY_STATUS)?;
    svc.start(&[] as &[&OsStr])
        .context("starting the service")?;
    let t = wait_for(&svc, ServiceState::Running)?;
    println!("service running after {} ms", t.as_millis());
    Ok(())
}

fn stop_svc(svc: &windows_service::service::Service) -> Result<()> {
    if svc.query_status()?.current_state == ServiceState::Stopped {
        return Ok(());
    }
    svc.stop().context("stopping the service")?;
    let t = wait_for(svc, ServiceState::Stopped)?;
    println!("service stopped after {} ms", t.as_millis());
    Ok(())
}

/// Stop the service and wait until it has stopped.
pub fn stop() -> Result<()> {
    stop_svc(&open(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS)?)
}

/// Stop (if running) and delete the service. The data dir (device key, journal, logs) is kept.
pub fn uninstall() -> Result<()> {
    let svc = open(ServiceAccess::STOP | ServiceAccess::QUERY_STATUS | ServiceAccess::DELETE)?;
    stop_svc(&svc)?;
    svc.delete().context("deleting the service")?;
    println!("service {SERVICE_NAME} removed; the data dir is kept (delete it by hand to wipe the device key)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_must_be_under_program_data() {
        let pd = Path::new(r"C:\ProgramData");
        assert!(check_data_dir(Path::new(r"C:\ProgramData\NetworkConnector"), pd).is_ok());
        assert!(check_data_dir(Path::new(r"C:\ProgramData"), pd).is_err());
        assert!(check_data_dir(Path::new(r"C:\Users\x\nc"), pd).is_err());
        assert!(check_data_dir(Path::new(r"C:\ProgramDataX\nc"), pd).is_err());
    }

    #[test]
    fn stopped_status_reports_failure_code() {
        let s = status(ServiceState::Stopped, 1, Duration::ZERO);
        assert_eq!(s.exit_code, ServiceExitCode::ServiceSpecific(1));
        assert!(s.controls_accepted.is_empty());
        let r = status(ServiceState::Running, 0, Duration::ZERO);
        assert!(r.controls_accepted.contains(ServiceControlAccept::STOP));
    }
}
