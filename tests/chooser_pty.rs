use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use expectrl::session::Session;
use expectrl::{Eof, Error};
use tempfile::TempDir;

fn make_fake_tmux_script(dir: &TempDir, script_body: &str) -> PathBuf {
    let path = dir.path().join("tmux");
    let mut file = fs::File::create(&path).expect("create fake tmux script");
    file.write_all(script_body.as_bytes())
        .expect("write fake tmux script");

    let mut perms = file.metadata().unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod fake tmux script");

    path
}

fn vmux_bin() -> String {
    // Prefer Cargo-provided binary path when available, otherwise fall back to
    // the default debug binary path for this crate.
    if let Ok(path) = std::env::var("CARGO_BIN_EXE_vmux") {
        return path;
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    format!("{manifest_dir}/target/debug/vmux")
}

fn spawn_in_pty(cmd: Command) -> Result<Session, Error> {
    Session::spawn(cmd)
}

fn with_fake_tmux_env(dir: &TempDir) -> Vec<(String, String)> {
    let mut envs = Vec::new();

    // Prepend the temp dir to PATH so our fake tmux is found first.
    let path = std::env::var("PATH").unwrap_or_default();
    envs.push((
        "PATH".to_string(),
        format!("{}:{}", dir.path().display(), path),
    ));

    envs
}

#[test]
fn chooser_attaches_session_outside_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");

    let log_path = dir.path().join("tmux_invocations.log");
    let script = format!(
        "#!/bin/sh\nset -eu\n
LOG=\"{}\"\n
echo \"tmux invoked: $0 $@\" >> \"$LOG\"\n
case \"$1\" in\n  list-sessions)\n    # Two unattached sessions. The UI will pick the first one.\n    echo 's1:1:0'\n    echo 's2:1:0'\n    ;;\n  attach-session)\n    # Record the full command-line for assertions.\n    echo \"$0 $@\" >> \"$LOG\"\n    ;;\n  switch-client)\n    echo \"unexpected switch-client outside TMUX: $0 $@\" >> \"$LOG\"\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
        log_path.display()
    );

    make_fake_tmux_script(&dir, &script);

    let bin = vmux_bin();
    let mut cmd = Command::new(&bin);

    for (k, v) in with_fake_tmux_env(&dir) {
        cmd.env(k, v);
    }

    // Ensure this test really exercises the non-TMUX branch even if the
    // surrounding environment is already inside tmux.
    cmd.env_remove("TMUX");

    // Drive vmux through the same adapter/selection path, but bypass the
    // interactive TUI in tests for robustness.
    cmd.env("VMUX_TEST_AUTOSELECT", "1");

    let mut pty = spawn_in_pty(cmd).expect("spawn vmux in pty");
    pty.expect(Eof).expect("vmux should exit cleanly");

    let log = fs::read_to_string(&log_path).expect("read tmux log");
    // We expect an attach-session targeting s1 (the first session).
    assert!(log.contains("attach-session"));
    assert!(log.contains("-t s1"));
}

#[test]
fn chooser_switches_client_inside_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");

    let log_path = dir.path().join("tmux_invocations.log");
    let script = format!(
        "#!/bin/sh\nset -eu\n
LOG=\"{}\"\n
echo \"tmux invoked: $0 $@\" >> \"$LOG\"\n
case \"$1\" in\n  list-sessions)\n    # One attached, one unattached. The UI should prefer the attached session.\n    echo 'alpha:1:0'\n    echo 'beta:1:1'\n    ;;\n  attach-session)\n    echo \"unexpected attach-session inside TMUX: $0 $@\" >> \"$LOG\"\n    ;;\n  switch-client)\n    echo \"$0 $@\" >> \"$LOG\"\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
        log_path.display()
    );

    make_fake_tmux_script(&dir, &script);

    let bin = vmux_bin();
    let mut cmd = Command::new(&bin);

    for (k, v) in with_fake_tmux_env(&dir) {
        cmd.env(k, v);
    }

    // Simulate running inside tmux.
    cmd.env("TMUX", "1");

    // Drive vmux through the same adapter/selection path, but bypass the
    // interactive TUI in tests for robustness.
    cmd.env("VMUX_TEST_AUTOSELECT", "1");

    let mut pty = spawn_in_pty(cmd).expect("spawn vmux in pty");
    pty.expect(Eof).expect("vmux should exit cleanly");

    let log = fs::read_to_string(&log_path).expect("read tmux log");

    // We expect switch-client to the attached session `beta`.
    assert!(log.contains("switch-client"));
    assert!(log.contains("-t beta"));
}
