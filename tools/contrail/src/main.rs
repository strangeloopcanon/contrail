use anyhow::{Context, Result, bail};
use std::env;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const PROC_CORE_DAEMON: ManagedProcess = ManagedProcess {
    name: "core_daemon",
    binary: "core_daemon",
    binary_env: "CONTRAIL_CORE_DAEMON_BIN",
    pid_file: "core_daemon.pid",
    log_file: "core_daemon.log",
    health_addr: None,
};

const PROC_DASHBOARD: ManagedProcess = ManagedProcess {
    name: "dashboard",
    binary: "dashboard",
    binary_env: "CONTRAIL_DASHBOARD_BIN",
    pid_file: "dashboard.pid",
    log_file: "dashboard.log",
    health_addr: Some("127.0.0.1:3000"),
};

const PROC_ANALYSIS: ManagedProcess = ManagedProcess {
    name: "analysis",
    binary: "analysis",
    binary_env: "CONTRAIL_ANALYSIS_BIN",
    pid_file: "analysis.pid",
    log_file: "analysis.log",
    health_addr: Some("127.0.0.1:3210"),
};

const PROCS_START_ORDER: [ManagedProcess; 3] = [PROC_CORE_DAEMON, PROC_DASHBOARD, PROC_ANALYSIS];
const PROCS_STOP_ORDER: [ManagedProcess; 3] = [PROC_ANALYSIS, PROC_DASHBOARD, PROC_CORE_DAEMON];

fn main() -> Result<()> {
    let args: Vec<OsString> = env::args_os().collect();
    if let Some(cmd) = parse_lifecycle_command(&args) {
        return run_lifecycle_command(cmd);
    }

    importer::run()
}

#[derive(Clone, Copy)]
enum LifecycleCommand {
    Up,
    Down,
    Status,
}

#[derive(Clone, Copy)]
struct ManagedProcess {
    name: &'static str,
    binary: &'static str,
    binary_env: &'static str,
    pid_file: &'static str,
    log_file: &'static str,
    health_addr: Option<&'static str>,
}

fn parse_lifecycle_command(args: &[OsString]) -> Option<LifecycleCommand> {
    let command = args.get(1)?.to_str()?;
    match command {
        "up" => Some(LifecycleCommand::Up),
        "down" => Some(LifecycleCommand::Down),
        "status" => Some(LifecycleCommand::Status),
        _ => None,
    }
}

fn run_lifecycle_command(command: LifecycleCommand) -> Result<()> {
    let run_dir = contrail_root_dir()?.join("run");
    fs::create_dir_all(&run_dir)
        .with_context(|| format!("failed to create run directory at {}", run_dir.display()))?;

    match command {
        LifecycleCommand::Up => {
            let mut started: Vec<ManagedProcess> = Vec::new();
            for process in PROCS_START_ORDER {
                if let Err(err) = start_process(&run_dir, process) {
                    for started_process in started.iter().rev() {
                        let _ = stop_process(&run_dir, *started_process);
                    }
                    return Err(err);
                }
                started.push(process);
            }
        }
        LifecycleCommand::Down => {
            for process in PROCS_STOP_ORDER {
                stop_process(&run_dir, process)?;
            }
        }
        LifecycleCommand::Status => {
            for process in PROCS_START_ORDER {
                print_process_status(&run_dir, process);
            }
        }
    }

    Ok(())
}

fn contrail_root_dir() -> Result<PathBuf> {
    if let Some(root) = env::var_os("CONTRAIL_HOME") {
        return Ok(PathBuf::from(root));
    }

    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set and CONTRAIL_HOME was not provided")?;
    Ok(home.join(".contrail"))
}

fn start_process(run_dir: &Path, process: ManagedProcess) -> Result<()> {
    let pid_path = run_dir.join(process.pid_file);
    let log_path = run_dir.join(process.log_file);

    if let Some(pid) = read_pid(&pid_path) {
        if is_pid_running(pid) {
            println!("{} already running (pid {})", process.name, pid);
            return Ok(());
        }
        fs::remove_file(&pid_path).ok();
    }

    let stdout_log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("failed to open log file {}", log_path.display()))?;
    let stderr_log = stdout_log
        .try_clone()
        .with_context(|| format!("failed to clone log file handle {}", log_path.display()))?;

    let binary = resolve_binary_path(process)?;
    let mut command = Command::new(&binary);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            bail!(
                "{} binary not found in PATH. Install it, then retry.",
                process.binary
            )
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to start {}", process.binary));
        }
    };

    let pid = child.id();
    fs::write(&pid_path, format!("{pid}\n"))
        .with_context(|| format!("failed to write pid file {}", pid_path.display()))?;
    println!(
        "started {} (pid {}, binary {}, log {})",
        process.name,
        pid,
        binary.display(),
        log_path.display()
    );

    let became_healthy = if let Some(addr) = process.health_addr {
        wait_for_health(process.name, addr)
    } else {
        true
    };

    if !became_healthy {
        if !is_pid_running(pid) {
            bail!(
                "{} exited before becoming healthy. Check {}. If a different `{}` binary is installed, set {} to the intended binary path.",
                process.name,
                log_path.display(),
                process.binary,
                process.binary_env
            );
        }
        bail!(
            "{} did not become healthy within timeout. Check {}. If a different `{}` binary is installed, set {} to the intended binary path.",
            process.name,
            log_path.display(),
            process.binary,
            process.binary_env
        );
    } else if !is_pid_running(pid) {
        bail!(
            "{} exited shortly after start. Check {}",
            process.name,
            log_path.display()
        );
    }

    Ok(())
}

fn stop_process(run_dir: &Path, process: ManagedProcess) -> Result<()> {
    let pid_path = run_dir.join(process.pid_file);

    let Some(pid) = read_pid(&pid_path) else {
        println!("{} not running", process.name);
        return Ok(());
    };

    if !is_pid_running(pid) {
        fs::remove_file(&pid_path).ok();
        println!("{} not running", process.name);
        return Ok(());
    }

    let _ = send_signal(pid, None)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !is_pid_running(pid) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    if is_pid_running(pid) {
        let killed = send_signal(pid, Some("-9"))?;
        if !killed && is_pid_running(pid) {
            bail!("failed to stop {} (pid {})", process.name, pid);
        }
    }

    fs::remove_file(&pid_path).ok();
    println!("stopped {} (pid {})", process.name, pid);
    Ok(())
}

fn print_process_status(run_dir: &Path, process: ManagedProcess) {
    let pid_path = run_dir.join(process.pid_file);
    match read_pid(&pid_path) {
        Some(pid) if is_pid_running(pid) => {
            println!("{}: running (pid {})", process.name, pid);
        }
        Some(_) => {
            fs::remove_file(&pid_path).ok();
            println!("{}: stopped", process.name);
        }
        None => {
            println!("{}: stopped", process.name);
        }
    }
}

fn read_pid(pid_path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(pid_path).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn is_pid_running(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn send_signal(pid: u32, signal: Option<&str>) -> Result<bool> {
    let mut command = Command::new("kill");
    if let Some(signal) = signal {
        command.arg(signal);
    }
    let status = command
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("failed to send signal to pid {}", pid))?;
    Ok(status.success())
}

fn wait_for_health(name: &str, addr: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if TcpStream::connect(addr).is_ok() {
            println!("{} healthy at http://{}", name, addr);
            return true;
        }
        thread::sleep(Duration::from_millis(500));
    }
    eprintln!(
        "warning: {} did not become healthy at http://{}",
        name, addr
    );
    false
}

fn resolve_binary_path(process: ManagedProcess) -> Result<PathBuf> {
    if let Some(path) = env::var_os(process.binary_env)
        && !path.is_empty()
    {
        return Ok(PathBuf::from(path));
    }

    if let Ok(current_exe) = env::current_exe()
        && let Some(bin_dir) = current_exe.parent()
    {
        let sibling = bin_dir.join(process.binary);
        if sibling.is_file() {
            return Ok(sibling);
        }
    }

    Ok(PathBuf::from(process.binary))
}

#[cfg(test)]
mod tests {
    use super::{LifecycleCommand, parse_lifecycle_command};
    use std::ffi::OsString;

    #[test]
    fn parses_lifecycle_commands() {
        let args = vec![OsString::from("contrail"), OsString::from("up")];
        assert!(matches!(
            parse_lifecycle_command(&args),
            Some(LifecycleCommand::Up)
        ));

        let args = vec![OsString::from("contrail"), OsString::from("down")];
        assert!(matches!(
            parse_lifecycle_command(&args),
            Some(LifecycleCommand::Down)
        ));

        let args = vec![OsString::from("contrail"), OsString::from("status")];
        assert!(matches!(
            parse_lifecycle_command(&args),
            Some(LifecycleCommand::Status)
        ));
    }

    #[test]
    fn leaves_other_commands_for_importer_cli() {
        let args = vec![OsString::from("contrail"), OsString::from("import-history")];
        assert!(parse_lifecycle_command(&args).is_none());
    }
}
