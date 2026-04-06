use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use expectrl::session::Session;
use expectrl::{Error, Regex};
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
    let path = std::env::var("PATH").unwrap_or_default();
    envs.push((
        "PATH".to_string(),
        format!("{}:{}", dir.path().display(), path),
    ));
    envs
}

#[test]
fn split_view_renders_sidebar_and_embedded_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");

    let log_path = dir.path().join("tmux_invocations.log");
    let script = format!(
        "#!/bin/sh\nset -eu\n\nLOG=\"{}\"\n\necho \"tmux invoked: $0 $@\" >> \"$LOG\"\ncase \"$1\" in\n  list-sessions)\n    echo 's1:1:0'\n    echo 's2:1:0'\n    ;;\n  attach-session)\n    printf '\\033[38;5;196mTMUX_SESSION:s1\\033[0m\\n'\n    # This fake client is intentionally short-lived: it reads one byte so the test can\n    # verify passthrough, then exits to exercise the embedded-client shutdown path.\n    stty raw -echo\n    key=\"$(dd bs=1 count=1 2>/dev/null | od -An -t x1 | tr -d ' \\n')\"\n    printf 'KEY:%s\\n' \"$key\" >> \"$LOG\"\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
        log_path.display()
    );

    make_fake_tmux_script(&dir, &script);

    let bin = vmux_bin();
    let mut cmd = Command::new(&bin);

    for (k, v) in with_fake_tmux_env(&dir) {
        cmd.env(k, v);
    }

    cmd.env_remove("TMUX");

    let mut pty = spawn_in_pty(cmd).expect("spawn vmux in pty");

    // The fake tmux client prints a colored sentinel line so we can prove the right-hand pane is live.
    pty.expect(Regex("\\x1b\\[[0-9;]*mTMUX_SESSION:s1"))
        .expect("embedded tmux pane should preserve tmux color output");

    // vmux should not intercept the keyboard anymore; q must reach tmux.
    pty.send("q").expect("send passthrough key to tmux");
    std::thread::sleep(std::time::Duration::from_secs(2));

    let log = fs::read_to_string(&log_path).expect("read tmux invocation log");
    assert!(log.contains("list-sessions"), "vmux should list tmux sessions");
    assert!(log.contains("attach-session"), "vmux should attach the selected session");
    assert!(log.contains("KEY:71"), "q should be forwarded to tmux, not intercepted by vmux");
}
