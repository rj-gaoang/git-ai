#![cfg(windows)]

#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

use repos::test_repo::{
    DaemonTestScope, GitTestMode, TestRepo, get_binary_path, real_git_executable,
};
use serde_json::Value;
use serial_test::serial;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

struct CommandResult {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

fn install_script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("install.ps1")
}

fn installed_git_ai_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("bin")
        .join("git-ai.exe")
}

fn installed_launcher_git_ai_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("launcher")
        .join("git-ai.exe")
}

fn installed_launcher_git_wrapper_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("launcher")
        .join("git.exe")
}

fn installed_current_exe_pointer_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path().join(".git-ai").join("current-exe")
}

fn installed_git_wrapper_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("bin")
        .join("git.exe")
}

fn foreground_daemon_stdout_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path().join("foreground-daemon.stdout.log")
}

fn foreground_daemon_stderr_path(repo: &TestRepo) -> PathBuf {
    repo.test_home_path().join("foreground-daemon.stderr.log")
}

fn foreground_daemon_logs(repo: &TestRepo) -> (String, String) {
    let stdout = fs::read_to_string(foreground_daemon_stdout_path(repo)).unwrap_or_default();
    let stderr = fs::read_to_string(foreground_daemon_stderr_path(repo)).unwrap_or_default();
    (stdout, stderr)
}

fn daemon_log_dir(repo: &TestRepo) -> PathBuf {
    repo.test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("logs")
}

fn wait_for_daemon_log_file(repo: &TestRepo, timeout: Duration) -> PathBuf {
    let deadline = Instant::now() + timeout;
    let pid_meta_path = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("daemon.pid.json");

    loop {
        if pid_meta_path.exists() {
            let meta_raw =
                fs::read_to_string(&pid_meta_path).expect("failed to read daemon pid metadata");
            let meta: Value =
                serde_json::from_str(&meta_raw).expect("failed to parse daemon pid metadata");
            let pid = meta
                .get("pid")
                .and_then(Value::as_u64)
                .expect("daemon pid metadata missing pid");
            let log_path = daemon_log_dir(repo).join(format!("{}.log", pid));
            if log_path.exists() {
                return log_path;
            }
        }

        if Instant::now() >= deadline {
            panic!(
                "daemon log file was not created within {:?} under {}",
                timeout,
                daemon_log_dir(repo).display()
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_file_to_contain(path: &PathBuf, needle: &str, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        let contents = fs::read_to_string(path).unwrap_or_default();
        if contents.contains(needle) {
            return;
        }

        if Instant::now() >= deadline {
            panic!(
                "file {} did not contain {:?} within {:?}\ncontents:\n{}",
                path.display(),
                needle,
                timeout,
                contents
            );
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn configure_install_env(command: &mut Command, repo: &TestRepo) {
    let home = repo.test_home_path().to_string_lossy().to_string();
    let (home_drive, home_path) = if home.len() >= 2 && home.as_bytes()[1] == b':' {
        (home[..2].to_string(), home[2..].to_string())
    } else {
        ("".to_string(), home.clone())
    };
    let git_dir = PathBuf::from(real_git_executable())
        .parent()
        .expect("real git executable should have a parent")
        .to_path_buf();
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path_with_git = if path.is_empty() {
        git_dir.into_os_string()
    } else {
        let mut combined = git_dir.into_os_string();
        combined.push(";");
        combined.push(path);
        combined
    };

    command.env("GIT_AI_LOCAL_BINARY", get_binary_path());
    command.env("GIT_AI_SKIP_PATH_UPDATE", "1");
    command.env("PATH", path_with_git);
    command.env("HOME", repo.test_home_path());
    command.env("USERPROFILE", repo.test_home_path());
    command.env("HOMEDRIVE", home_drive);
    command.env("HOMEPATH", home_path);
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    command.env(
        "APPDATA",
        repo.test_home_path().join("AppData").join("Roaming"),
    );
    command.env(
        "LOCALAPPDATA",
        repo.test_home_path().join("AppData").join("Local"),
    );
    command.env("GIT_AI_TEST_DB_PATH", repo.test_db_path());
    command.env("GITAI_TEST_DB_PATH", repo.test_db_path());
    command.env("GIT_AI_DAEMON_HOME", repo.daemon_home_path());
    command.env(
        "GIT_AI_DAEMON_CONTROL_SOCKET",
        repo.daemon_control_socket_path(),
    );
    command.env(
        "GIT_AI_DAEMON_TRACE_SOCKET",
        repo.daemon_trace_socket_path(),
    );
}

fn run_command_with_timeout(command: &mut Command, timeout: Duration) -> CommandResult {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("failed to spawn command");
    let deadline = Instant::now() + timeout;

    loop {
        match child.try_wait().expect("failed to poll child status") {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut handle) = child.stdout.take() {
                    let _ = handle.read_to_string(&mut stdout);
                }
                if let Some(mut handle) = child.stderr.take() {
                    let _ = handle.read_to_string(&mut stderr);
                }
                return CommandResult {
                    status,
                    stdout,
                    stderr,
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();

                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut handle) = child.stdout.take() {
                    let _ = handle.read_to_string(&mut stdout);
                }
                if let Some(mut handle) = child.stderr.take() {
                    let _ = handle.read_to_string(&mut stderr);
                }

                panic!(
                    "command timed out after {:?}\nstdout:\n{}\nstderr:\n{}",
                    timeout, stdout, stderr
                );
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn run_install_script(repo: &TestRepo, timeout: Duration) -> CommandResult {
    run_install_script_with_extra_env(repo, timeout, &[])
}

fn run_install_script_with_extra_env(
    repo: &TestRepo,
    timeout: Duration,
    extra_env: &[(&str, &str)],
) -> CommandResult {
    let mut command = Command::new("powershell");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(install_script_path())
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    configure_install_env(&mut command, repo);
    for (key, value) in extra_env {
        command.env(key, value);
    }
    run_command_with_timeout(&mut command, timeout)
}

fn run_installed_git_ai(repo: &TestRepo, args: &[&str], timeout: Duration) -> CommandResult {
    run_git_ai_at_path(repo, installed_git_ai_path(repo), args, timeout)
}

fn run_git_ai_at_path(
    repo: &TestRepo,
    git_ai_path: PathBuf,
    args: &[&str],
    timeout: Duration,
) -> CommandResult {
    let mut command = Command::new(git_ai_path);
    command.args(args).current_dir(repo.test_home_path());
    configure_install_env(&mut command, repo);
    run_command_with_timeout(&mut command, timeout)
}

fn spawn_installed_daemon(repo: &TestRepo) -> Child {
    let stdout_log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(foreground_daemon_stdout_path(repo))
        .expect("failed to create daemon stdout log");
    let stderr_log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(foreground_daemon_stderr_path(repo))
        .expect("failed to create daemon stderr log");
    let mut command = Command::new(installed_launcher_git_ai_path(repo));
    command
        .args(["bg", "run"])
        .current_dir(repo.test_home_path())
        .stdout(Stdio::from(stdout_log))
        .stderr(Stdio::from(stderr_log));
    configure_install_env(&mut command, repo);
    command.spawn().expect("failed to spawn installed daemon")
}

fn kill_installed_processes(repo: &TestRepo) {
    let script = format!(
        "$targets = @('{}','{}','{}','{}'); \
         Get-CimInstance Win32_Process -ErrorAction SilentlyContinue | \
         Where-Object {{ $_.ExecutablePath -and ($targets -contains $_.ExecutablePath) }} | \
         ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}",
        installed_git_ai_path(repo).display(),
        installed_launcher_git_ai_path(repo).display(),
        installed_launcher_git_wrapper_path(repo).display(),
        repo.test_home_path()
            .join(".git-ai")
            .join("bin")
            .join("git.exe")
            .display()
    );
    let _ = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn wait_for_child_to_stay_alive(repo: &TestRepo, child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("failed to poll foreground daemon") {
            Some(status) => {
                let (stdout, stderr) = foreground_daemon_logs(repo);
                panic!(
                    "foreground daemon exited before reinstall started: {}\nstdout:\n{}\nstderr:\n{}",
                    status, stdout, stderr
                );
            }
            None if Instant::now() >= deadline => return,
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn wait_for_child_exit(repo: &TestRepo, child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("failed to poll foreground daemon") {
            Some(_) => return,
            None if Instant::now() >= deadline => {
                let (stdout, stderr) = foreground_daemon_logs(repo);
                panic!(
                    "foreground daemon was not stopped by reinstall within {:?}\nstdout:\n{}\nstderr:\n{}",
                    timeout, stdout, stderr
                );
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[test]
#[serial]
fn windows_install_script_reinstall_stops_running_daemon() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::NoDaemon);

    let initial_install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        initial_install.status.success(),
        "initial install should succeed\nstdout:\n{}\nstderr:\n{}",
        initial_install.stdout,
        initial_install.stderr
    );

    let installed_git_ai = installed_git_ai_path(&repo);
    assert!(
        installed_git_ai.exists(),
        "git-ai.exe should be installed at {}",
        installed_git_ai.display()
    );

    let mut daemon = spawn_installed_daemon(&repo);
    wait_for_child_to_stay_alive(&repo, &mut daemon, Duration::from_secs(2));

    let reinstall = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        reinstall.status.success(),
        "reinstall with daemon running should succeed\nstdout:\n{}\nstderr:\n{}",
        reinstall.stdout,
        reinstall.stderr
    );

    wait_for_child_exit(&repo, &mut daemon, Duration::from_secs(20));

    let version = run_installed_git_ai(&repo, &["--version"], Duration::from_secs(15));
    assert!(
        version.status.success(),
        "installed git-ai should remain usable after reinstall\nstdout:\n{}\nstderr:\n{}",
        version.stdout,
        version.stderr
    );

    kill_installed_processes(&repo);
}

#[test]
#[serial]
fn windows_install_script_passive_update_retires_busy_exe() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::NoDaemon);

    let initial_install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        initial_install.status.success(),
        "initial install should succeed\nstdout:\n{}\nstderr:\n{}",
        initial_install.stdout,
        initial_install.stderr
    );

    let installed_git_ai = installed_git_ai_path(&repo);
    assert!(
        installed_git_ai.exists(),
        "git-ai.exe should be installed at {}",
        installed_git_ai.display()
    );

    let mut daemon = spawn_installed_daemon(&repo);
    wait_for_child_to_stay_alive(&repo, &mut daemon, Duration::from_secs(2));

    let reinstall = run_install_script_with_extra_env(
        &repo,
        Duration::from_secs(90),
        &[("GIT_AI_DEFER_IF_BUSY", "1")],
    );
    assert!(
        reinstall.status.success(),
        "passive reinstall with daemon running should succeed\nstdout:\n{}\nstderr:\n{}",
        reinstall.stdout,
        reinstall.stderr
    );
    assert!(
        reinstall
            .stdout
            .contains("Retired active launcher git-ai.exe before install"),
        "installer should retire the busy launcher exe instead of deferring\nstdout:\n{}\nstderr:\n{}",
        reinstall.stdout,
        reinstall.stderr
    );

    let version = run_installed_git_ai(&repo, &["--version"], Duration::from_secs(15));
    assert!(
        version.status.success(),
        "installed git-ai should remain usable after passive reinstall\nstdout:\n{}\nstderr:\n{}",
        version.stdout,
        version.stderr
    );

    let _ = daemon.kill();
    let _ = daemon.wait();
    kill_installed_processes(&repo);
}

#[test]
#[serial]
fn windows_daemon_creates_log_file() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::NoDaemon);

    let initial_install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        initial_install.status.success(),
        "initial install should succeed\nstdout:\n{}\nstderr:\n{}",
        initial_install.stdout,
        initial_install.stderr
    );

    let mut daemon = spawn_installed_daemon(&repo);
    wait_for_child_to_stay_alive(&repo, &mut daemon, Duration::from_secs(2));

    let log_path = wait_for_daemon_log_file(&repo, Duration::from_secs(15));
    wait_for_file_to_contain(&log_path, "daemon log initialized", Duration::from_secs(15));

    kill_installed_processes(&repo);
    let _ = daemon.wait();
}

#[test]
#[serial]
fn windows_install_script_synchronizes_launcher_current_exe_and_compat_bin() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::NoDaemon);

    let install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        install.status.success(),
        "install should succeed\nstdout:\n{}\nstderr:\n{}",
        install.stdout,
        install.stderr
    );

    let launcher = installed_launcher_git_ai_path(&repo);
    let launcher_git = installed_launcher_git_wrapper_path(&repo);
    let compat_bin = installed_git_ai_path(&repo);
    let compat_git = installed_git_wrapper_path(&repo);
    let pointer = installed_current_exe_pointer_path(&repo);
    assert!(
        launcher.exists(),
        "launcher git-ai.exe should be installed at {}",
        launcher.display()
    );
    assert!(
        launcher_git.exists(),
        "launcher git proxy should be installed at {}",
        launcher_git.display()
    );
    assert!(
        compat_bin.exists(),
        "compatibility git-ai.exe should be installed at {}",
        compat_bin.display()
    );
    assert!(
        compat_git.exists(),
        "compatibility git proxy should be installed at {}",
        compat_git.display()
    );
    assert!(
        pointer.exists(),
        "current-exe pointer should be written at {}",
        pointer.display()
    );

    let pointer_value = fs::read_to_string(&pointer)
        .expect("failed to read current-exe pointer")
        .trim()
        .to_string();
    assert_eq!(
        fs::canonicalize(&pointer_value).expect("current-exe target should exist"),
        fs::canonicalize(&launcher).expect("launcher should canonicalize"),
        "current-exe should point at the launcher entrypoint"
    );

    let launcher_version = run_git_ai_at_path(
        &repo,
        launcher.clone(),
        &["--version"],
        Duration::from_secs(15),
    );
    let compat_version =
        run_git_ai_at_path(&repo, compat_bin, &["--version"], Duration::from_secs(15));
    assert!(
        launcher_version.status.success(),
        "launcher --version should succeed\nstdout:\n{}\nstderr:\n{}",
        launcher_version.stdout,
        launcher_version.stderr
    );
    assert!(
        compat_version.status.success(),
        "compatibility bin --version should succeed\nstdout:\n{}\nstderr:\n{}",
        compat_version.stdout,
        compat_version.stderr
    );
    assert_eq!(
        launcher_version.stdout.trim(),
        compat_version.stdout.trim(),
        "launcher and compatibility bin should report the same version"
    );
    assert_eq!(
        fs::read(&launcher).expect("failed to read launcher git-ai.exe"),
        fs::read(&launcher_git).expect("failed to read launcher git.exe"),
        "launcher git.exe should be byte-for-byte synchronized with launcher git-ai.exe"
    );
    assert_eq!(
        fs::read(&launcher).expect("failed to read launcher git-ai.exe"),
        fs::read(&compat_git).expect("failed to read compatibility git.exe"),
        "compatibility git.exe should be byte-for-byte synchronized from launcher"
    );
    assert!(
        install
            .stdout
            .contains("Synchronized git-ai and git proxy entrypoints into"),
        "installer should report entrypoint sync\nstdout:\n{}",
        install.stdout
    );
}

fn seed_existing_wrapper(repo: &TestRepo) {
    let bin_dir = repo.test_home_path().join(".git-ai").join("bin");
    fs::create_dir_all(&bin_dir).expect("failed to create install dir");
    fs::write(bin_dir.join("git-ai.exe"), b"").expect("failed to create git-ai.exe stub");
    fs::write(bin_dir.join("git.exe"), b"").expect("failed to create git.exe stub");
}

#[test]
#[serial]
fn windows_install_script_installs_proxy_for_new_users() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::NoDaemon);

    let install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        install.status.success(),
        "fresh install should succeed\nstdout:\n{}\nstderr:\n{}",
        install.stdout,
        install.stderr
    );

    assert!(
        installed_git_ai_path(&repo).exists(),
        "git-ai.exe should be installed at {}",
        installed_git_ai_path(&repo).display()
    );

    assert!(
        installed_launcher_git_wrapper_path(&repo).exists(),
        "fresh install should create the launcher git proxy"
    );
    assert!(
        installed_git_wrapper_path(&repo).exists(),
        "fresh install should create the compatibility git proxy"
    );
    let bin_dir = repo.test_home_path().join(".git-ai").join("bin");
    assert!(
        !bin_dir.join("git-og.cmd").exists(),
        "fresh install should NOT create git-og.cmd"
    );
}

#[test]
#[serial]
fn windows_install_script_refreshes_wrapper_for_existing_users() {
    let repo =
        TestRepo::new_with_mode_and_daemon_scope(GitTestMode::Daemon, DaemonTestScope::NoDaemon);

    seed_existing_wrapper(&repo);

    let install = run_install_script(&repo, Duration::from_secs(90));
    assert!(
        install.status.success(),
        "existing-wrapper install should succeed\nstdout:\n{}\nstderr:\n{}",
        install.stdout,
        install.stderr
    );

    assert!(
        installed_git_wrapper_path(&repo).exists(),
        "existing git.exe wrapper should be refreshed"
    );

    assert_eq!(
        fs::read(installed_git_ai_path(&repo)).expect("failed to read compatibility git-ai.exe"),
        fs::read(installed_git_wrapper_path(&repo)).expect("failed to read compatibility git.exe"),
        "existing git.exe wrapper should be synchronized with git-ai.exe"
    );
}

#[test]
fn windows_install_script_does_not_shadow_reserved_pid_variable() {
    let script = fs::read_to_string(install_script_path()).expect("failed to read install.ps1");
    assert!(
        !script.contains("foreach ($pid in $pids)"),
        "install.ps1 should not iterate with the reserved $PID variable name"
    );
    assert!(
        script.contains("foreach ($processId in $processIds)"),
        "install.ps1 should use a non-reserved loop variable for managed process ids"
    );
}

#[test]
fn windows_install_script_gates_daemon_restart_to_self_update() {
    let script = fs::read_to_string(install_script_path()).expect("failed to read install.ps1");
    assert!(
        script.contains("GIT_AI_RESTART_DAEMON_AFTER_INSTALL"),
        "install.ps1 should only restart the daemon when the self-update env flag is set"
    );
    assert!(
        script.contains("Start-DaemonIfRequested"),
        "install.ps1 should funnel daemon restart attempts through the gated helper"
    );
}

#[test]
fn windows_install_script_replaces_busy_binary_by_retiring_it() {
    let script = fs::read_to_string(install_script_path()).expect("failed to read install.ps1");
    assert!(
        script.contains("function Install-BinaryWithRenameFallback"),
        "install.ps1 should install through the busy-binary rename fallback"
    );
    assert!(
        script.contains(
            "Install-BinaryWithRenameFallback -Source $tmpFile -Destination $launcherExe"
        ),
        "install.ps1 should replace launcher git-ai.exe via the rename fallback"
    );
    assert!(
        script.contains("Copy-InstalledBinary -Source $launcherExe -Destination $finalExe"),
        "install.ps1 should synchronize the compatibility bin entrypoint from launcher"
    );
    assert!(
        script.contains("Copy-InstalledBinary -Source $launcherExe -Destination $launcherGitShim"),
        "install.ps1 should synchronize the launcher git proxy from launcher"
    );
    assert!(
        script.contains("Copy-InstalledBinary -Source $launcherExe -Destination $gitShim"),
        "install.ps1 should synchronize the compatibility git proxy from launcher"
    );
    assert!(
        script.contains("Retired active $Description before install"),
        "install.ps1 should explain when it retires an active binary"
    );
    assert!(
        script.contains("Could not retire active $Description before stopping processes"),
        "install.ps1 should only stop processes after rename-retire fails"
    );
    let retire_before_kill = script
        .find("Retired active $Description before install")
        .expect("rename-retire warning should be present");
    let kill_after_retire_fallback = script
        .find("Could not retire active $Description before stopping processes")
        .expect("kill fallback warning should be present");
    assert!(
        retire_before_kill < kill_after_retire_fallback,
        "install.ps1 should try rename-retire before process-kill fallback"
    );
    assert!(
        !script.contains("Move-Item -Force -Path $tmpFile -Destination $finalExe"),
        "install.ps1 should not directly overwrite git-ai.exe after waiting for a write handle"
    );
}

#[test]
fn windows_install_script_writes_current_exe_pointer_to_launcher() {
    let script = fs::read_to_string(install_script_path()).expect("failed to read install.ps1");
    assert!(
        script.contains("function Set-CurrentExePointer"),
        "install.ps1 should write the current-exe pointer"
    );
    assert!(
        script.contains("$launcherDir = Join-Path $gitAiRoot 'launcher'"),
        "install.ps1 should install the authoritative launcher entrypoint"
    );
    assert!(
        script.contains("Set-PathEnsureContains -PathToAdd $launcherDir"),
        "install.ps1 should put the stable launcher entrypoint on PATH"
    );
    assert!(
        !script.contains("Set-PathEnsureContains -PathToAdd $installDir"),
        "install.ps1 should not prefer the compatibility bin entrypoint on PATH"
    );
    assert!(
        script.contains("$currentExePointer = Join-Path $gitAiRoot 'current-exe'"),
        "install.ps1 should maintain the current-exe pointer path"
    );
    assert!(
        script.contains(
            "Set-CurrentExePointer -PointerPath $currentExePointer -TargetPath $launcherExe"
        ),
        "install.ps1 should point current-exe at launcher"
    );
}

#[test]
fn windows_install_script_allows_git_ai_rust_install_probe_during_service_updates() {
    let script = fs::read_to_string(install_script_path()).expect("failed to read install.ps1");
    assert!(
        script.contains("function Invoke-GitAiInstallHooks"),
        "install.ps1 should wrap install-hooks so git-ai can run its own install probe"
    );
    assert!(
        script.contains("Remove-Item Env:GIT_AI_SKIP_INSTALL_TEST_UPLOAD"),
        "install.ps1 should temporarily clear inherited service skip env for install-hooks"
    );
    assert!(
        script.contains("$env:GIT_AI_DEFER_INSTALL_HOOKS_PROBE = '1'"),
        "install.ps1 should defer install-hooks probes until the full installer result is known"
    );
    assert!(
        script.contains("function Invoke-GitAiPostInstallProbe"),
        "install.ps1 should expose a standalone install probe wrapper"
    );
    assert!(
        script.contains("[string]::IsNullOrWhiteSpace($env:GIT_AI_TEST_DB_PATH)"),
        "install.ps1 should keep test database environments from uploading install probes"
    );
    assert!(
        script.contains("Invoke-GitAiInstallHooks -GitAiExe $launcherExe"),
        "install.ps1 should call install-hooks through the environment-isolating wrapper"
    );
    assert!(
        script.contains("Invoke-GitAiPostInstallProbe -GitAiExe $launcherExe -Status 'failed' -Stage 'install-hooks' -Reason $installHooksError"),
        "install.ps1 should send a distinguishable failed-install probe when hook setup fails"
    );
    assert!(
        script.contains("Invoke-GitAiPostInstallProbe -GitAiExe $launcherExe -Status 'success'"),
        "install.ps1 should send success probes only after the full installer reports success"
    );
    assert!(
        script.contains("$installHooksSucceeded = $false")
            && script.contains("$installHooksSucceeded = $true")
            && script.contains("if ($installHooksSucceeded)"),
        "install.ps1 should not send a success probe after hook setup failed"
    );
    assert!(
        script.contains("throw \"git-ai install-hooks exited with code $LASTEXITCODE\""),
        "install.ps1 should treat native install-hooks nonzero exit codes as hook setup failures"
    );
    assert!(
        script.contains("throw \"git-ai post-install-probe exited with code $LASTEXITCODE\""),
        "install.ps1 should treat native post-install-probe nonzero exit codes as probe failures"
    );
    let hook_call = script
        .find("Invoke-GitAiInstallHooks -GitAiExe $launcherExe")
        .expect("install-hooks wrapper call should exist");
    let probe_call = script
        .find("Invoke-GitAiPostInstallProbe -GitAiExe $launcherExe -Status 'success'")
        .expect("post-install-probe wrapper call should exist");
    assert!(
        hook_call < probe_call,
        "success install probe should run after hook setup and final installer success output"
    );
    assert!(
        !script.contains("& $launcherExe install-hooks | Out-Host"),
        "install.ps1 should not call install-hooks directly"
    );
}

#[test]
fn windows_install_script_stops_process_trees() {
    let script = fs::read_to_string(install_script_path()).expect("failed to read install.ps1");
    assert!(
        script.contains("function Stop-ProcessTree"),
        "install.ps1 should use a process-tree aware kill helper"
    );
    assert!(
        script.contains("taskkill.exe /F /T /PID"),
        "install.ps1 should use taskkill /T so hook child processes do not keep git-ai.exe locked"
    );
}
