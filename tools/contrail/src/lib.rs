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

pub fn run() -> Result<()> {
    let args: Vec<OsString> = env::args_os().collect();
    if args.get(1).and_then(|s| s.to_str()) == Some("history") {
        return run_native_history(&args[2..]);
    }
    if matches!(args.get(1).and_then(|s| s.to_str()), Some("--help" | "-h")) {
        println!("Native app backups: contrail history --help\n");
    }
    if let Some(cmd) = parse_lifecycle_command(&args) {
        return run_lifecycle_command(cmd);
    }

    importer::run()
}

// Embed the stdlib-only snapshot engine so installed binaries do not depend on
// a checkout. Python supplies SQLite's online backup API without extra crates.
fn run_native_history(args: &[OsString]) -> Result<()> {
    let engine = concat!(
        include_str!("../native_sources.py"),
        "\n",
        include_str!("../native_retention.py"),
        "\n",
        include_str!("../native_restore.py"),
        "\n",
        include_str!("../native_history.py")
    );
    let python = native_history_python()?;
    let status = Command::new(&python)
        .arg("-c")
        .arg(engine)
        .args(args)
        .status()
        .with_context(|| {
            format!(
                "failed to run native history with {}",
                python.to_string_lossy()
            )
        })?;
    if !status.success() {
        bail!("native history command failed ({status})");
    }
    Ok(())
}

// Prefer a decoder-capable runtime even when a project-local python3 is older.
// Probe only an in-memory database, never a user's profile or archive.
const NATIVE_PYTHON_PROBE: &str = r#"
import sys, sqlite3
if sys.version_info < (3, 9):
    raise RuntimeError('Python 3.9 or later is required')
if sqlite3.sqlite_version_info < (3, 27):
    raise RuntimeError('SQLite VACUUM INTO is required')
with sqlite3.connect(':memory:') as db:
    if db.execute('PRAGMA journal_mode=OFF').fetchone()[0] != 'off':
        raise RuntimeError('SQLite journal_mode=OFF is required')
try:
    from compression import zstd
except ImportError:
    print(sys.executable)
    sys.exit(10)
print(sys.executable)
"#;

fn native_history_python() -> Result<OsString> {
    select_native_history_python(
        env::var_os("CONTRAIL_HISTORY_PYTHON"),
        &[
            "python3".into(),
            "python3.14".into(),
            "/opt/homebrew/bin/python3.14".into(),
            "/usr/local/bin/python3.14".into(),
        ],
    )
}

fn probe_native_python(candidate: &OsString) -> Result<(OsString, bool)> {
    let output = Command::new(candidate)
        .args(["-c", NATIVE_PYTHON_PROBE])
        .output()
        .with_context(|| format!("could not run {}", candidate.to_string_lossy()))?;
    if !matches!(output.status.code(), Some(0 | 10)) {
        bail!(
            "{}: {}",
            candidate.to_string_lossy(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let executable = String::from_utf8(output.stdout)?;
    let executable = executable.trim_end_matches(['\r', '\n']);
    if executable.is_empty() {
        bail!("Python probe returned no executable");
    }
    Ok((executable.into(), output.status.success()))
}

fn select_native_history_python(
    override_python: Option<OsString>,
    candidates: &[OsString],
) -> Result<OsString> {
    if let Some(python) = override_python {
        return probe_native_python(&python)
            .map(|(executable, _)| executable)
            .context("CONTRAIL_HISTORY_PYTHON must name one compatible Python executable, without arguments");
    }
    let mut fallback = None;
    for candidate in candidates {
        if let Ok((executable, zstd)) = probe_native_python(candidate) {
            if zstd {
                return Ok(executable);
            }
            fallback.get_or_insert(executable);
        }
    }
    fallback.context("native history requires Python 3.9+ with SQLite 3.27+ and journal_mode=OFF; set CONTRAIL_HISTORY_PYTHON to a compatible executable (Python 3.14+ adds compression.zstd)")
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

    let _ = send_signal(pid, Some("-INT"))?;
    wait_until_stopped(pid, Duration::from_secs(5));

    if is_pid_running(pid) {
        let _ = send_signal(pid, None)?;
        wait_until_stopped(pid, Duration::from_secs(2));
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
    parse_pid(&raw)
}

fn parse_pid(raw: &str) -> Option<u32> {
    let pid = raw.trim().parse::<u32>().ok()?;
    (pid > 0).then_some(pid)
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

fn wait_until_stopped(pid: u32, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !is_pid_running(pid) {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
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
    use super::{LifecycleCommand, parse_lifecycle_command, parse_pid};
    use std::ffi::OsString;

    #[test]
    fn native_history_fixtures() {
        let status = std::process::Command::new(super::native_history_python().unwrap())
            .args([
                "-m",
                "unittest",
                "discover",
                "-s",
                env!("CARGO_MANIFEST_DIR"),
                "-p",
                "test_native_*.py",
                "-v",
            ])
            .status()
            .expect("Python 3 with sqlite3 is required for native history fixtures");
        assert!(status.success(), "native history fixtures failed");
    }

    #[test]
    fn native_history_embedded_help() {
        super::run_native_history(&[OsString::from("--help")]).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn native_python_selection_probes_candidates_and_honors_override() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let root = std::env::temp_dir().join(format!(
            "contrail-python-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        // These disposable executables stand in for runtimes. No process-wide
        // environment changes or installed Python versions are needed.
        let runtime = |name: &str, code: i32| -> OsString {
            let path = root.join(name);
            std::fs::write(
                &path,
                format!("#!/bin/sh\nprintf '%s\\n' \"$0\"\nexit {code}\n"),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path.into_os_string()
        };
        let legacy = runtime("python3", 10);
        let capable = runtime("python3.14", 0);
        let invalid = runtime("invalid", 1);
        let missing = root.join("missing").into_os_string();
        let shim = runtime("shim", 0);
        std::fs::write(&shim, "#!/bin/sh\nprintf '%s/python3.14\\n' \"${0%/*}\"\n").unwrap();
        let select = super::select_native_history_python;
        assert_eq!(select(Some(shim), &[]).unwrap(), capable);
        assert_eq!(
            select(
                None,
                &[
                    missing.clone(),
                    invalid.clone(),
                    legacy.clone(),
                    capable.clone()
                ]
            )
            .unwrap(),
            capable
        );
        assert_eq!(
            select(None, &[invalid.clone(), legacy.clone()]).unwrap(),
            legacy
        );
        assert_eq!(
            select(Some(legacy.clone()), std::slice::from_ref(&capable)).unwrap(),
            legacy
        );
        assert!(
            select(None, &[missing.clone(), invalid.clone()])
                .unwrap_err()
                .to_string()
                .contains("CONTRAIL_HISTORY_PYTHON")
        );
        for explicit in [invalid, missing, OsString::new()] {
            assert!(
                select(Some(explicit), std::slice::from_ref(&capable))
                    .unwrap_err()
                    .to_string()
                    .contains("CONTRAIL_HISTORY_PYTHON")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

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

    #[test]
    fn parse_pid_accepts_trimmed_positive_pid() {
        assert_eq!(parse_pid(" 42\n"), Some(42));
    }

    #[test]
    fn parse_pid_rejects_invalid_or_zero_pid() {
        assert_eq!(parse_pid("not-a-pid"), None);
        assert_eq!(parse_pid("0"), None);
        assert_eq!(parse_pid(""), None);
    }
}
