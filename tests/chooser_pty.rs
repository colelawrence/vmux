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

fn fake_list_sessions_output(names: &[&str]) -> String {
    names
        .iter()
        .map(|name| format!("printf '{name}\\t{name}\\t1\\t0\\n'"))
        .collect::<Vec<_>>()
        .join("\n    ")
}

#[test]
fn split_view_renders_sidebar_and_embedded_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");

    let log_path = dir.path().join("tmux_invocations.log");
    let script = format!(
        "#!/bin/sh\nset -eu\n\nLOG=\"{}\"\n\necho \"tmux invoked: $0 $@\" >> \"$LOG\"\ncase \"$1\" in\n  list-sessions)\n    {}\n    ;;\n  attach-session)\n    printf '\\033[38;5;196mTMUX_SESSION:s1\\033[0m\\n'\n    stty raw -echo\n    key=\"$(dd bs=1 count=1 2>/dev/null | od -An -t x1 | tr -d ' \\n')\"\n    printf 'KEY:%s\\n' \"$key\" >> \"$LOG\"\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
        log_path.display(),
        fake_list_sessions_output(&["s1", "s2"])
    );

    make_fake_tmux_script(&dir, &script);

    let bin = vmux_bin();
    let mut cmd = Command::new(&bin);

    for (k, v) in with_fake_tmux_env(&dir) {
        cmd.env(k, v);
    }

    cmd.env_remove("TMUX");

    let mut pty = spawn_in_pty(cmd).expect("spawn vmux in pty");
    pty.expect(Regex("\\x1b\\[[0-9;]*mTMUX_SESSION:s1"))
        .expect("embedded tmux pane should preserve tmux color output");

    pty.send("q").expect("send passthrough key to tmux");
    std::thread::sleep(std::time::Duration::from_secs(2));

    let log = fs::read_to_string(&log_path).expect("read tmux invocation log");
    assert!(
        log.contains("list-sessions"),
        "vmux should list tmux sessions"
    );
    assert!(
        log.contains("attach-session"),
        "vmux should attach the selected session"
    );
    assert!(
        log.contains("KEY:71"),
        "q should be forwarded to tmux, not intercepted by vmux"
    );
}

#[test]
fn split_view_passes_bare_escape_to_embedded_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");

    let log_path = dir.path().join("tmux_escape.log");
    let script = format!(
        "#!/bin/sh\nset -eu\n\nLOG=\"{}\"\n\necho \"tmux invoked: $0 $@\" >> \"$LOG\"\ncase \"$1\" in\n  list-sessions)\n    {}\n    ;;\n  attach-session)\n    printf 'READY\\n'\n    stty raw -echo\n    key=\"$(dd bs=1 count=1 2>/dev/null | od -An -t x1 | tr -d ' \\n')\"\n    printf 'KEY:%s\\n' \"$key\" >> \"$LOG\"\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
        log_path.display(),
        fake_list_sessions_output(&["s1"])
    );

    make_fake_tmux_script(&dir, &script);

    let bin = vmux_bin();
    let mut cmd = Command::new(&bin);

    for (k, v) in with_fake_tmux_env(&dir) {
        cmd.env(k, v);
    }

    cmd.env_remove("TMUX");

    let mut pty = spawn_in_pty(cmd).expect("spawn vmux in pty");
    pty.expect(Regex("READY"))
        .expect("embedded tmux pane should be ready");

    pty.send("\u{1b}")
        .expect("send bare escape byte to vmux host");
    std::thread::sleep(std::time::Duration::from_secs(2));

    let log = fs::read_to_string(&log_path).expect("read tmux invocation log");
    assert!(
        log.contains("KEY:1b"),
        "vmux should forward a bare escape byte unchanged to tmux"
    );
}

#[test]
fn split_view_passes_raw_keyboard_bytes_to_embedded_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");

    let log_path = dir.path().join("tmux_raw_bytes.log");
    let script = format!(
        "#!/bin/sh\nset -eu\n\nLOG=\"{}\"\n\necho \"tmux invoked: $0 $@\" >> \"$LOG\"\ncase \"$1\" in\n  list-sessions)\n    {}\n    ;;\n  attach-session)\n    printf 'READY\\n'\n    stty raw -echo\n    keys=\"$(dd bs=1 count=2 2>/dev/null | od -An -t x1 | tr -d ' \\n')\"\n    printf 'KEYS:%s\\n' \"$keys\" >> \"$LOG\"\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
        log_path.display(),
        fake_list_sessions_output(&["s1"])
    );

    make_fake_tmux_script(&dir, &script);

    let bin = vmux_bin();
    let mut cmd = Command::new(&bin);

    for (k, v) in with_fake_tmux_env(&dir) {
        cmd.env(k, v);
    }

    cmd.env_remove("TMUX");

    let mut pty = spawn_in_pty(cmd).expect("spawn vmux in pty");
    pty.expect(Regex("READY"))
        .expect("embedded tmux pane should be ready");

    pty.send("\u{1b}\r")
        .expect("send raw alt-enter byte sequence to tmux");
    std::thread::sleep(std::time::Duration::from_secs(2));

    let log = fs::read_to_string(&log_path).expect("read tmux invocation log");
    assert!(
        log.contains("KEYS:1b0d"),
        "vmux should forward raw keyboard bytes unchanged to tmux"
    );
}

#[test]
fn split_view_forwards_unclaimed_sgr_mouse_bytes_to_embedded_tmux() {
    let dir = tempfile::tempdir().expect("tempdir");

    let log_path = dir.path().join("tmux_mouse_bytes.log");
    let script = format!(
        "#!/bin/sh\nset -eu\n\nLOG=\"{}\"\n\necho \"tmux invoked: $0 $@\" >> \"$LOG\"\ncase \"$1\" in\n  list-sessions)\n    {}\n    ;;\n  attach-session)\n    printf 'READY\\n'\n    stty raw -echo\n    mouse=\"$(dd bs=1 count=13 2>/dev/null | od -An -t x1 | tr -d ' \\n')\"\n    printf 'MOUSE:%s\\n' \"$mouse\" >> \"$LOG\"\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
        log_path.display(),
        fake_list_sessions_output(&["s1"])
    );

    make_fake_tmux_script(&dir, &script);

    let bin = vmux_bin();
    let mut cmd = Command::new(&bin);

    for (k, v) in with_fake_tmux_env(&dir) {
        cmd.env(k, v);
    }

    cmd.env_remove("TMUX");

    let mut pty = spawn_in_pty(cmd).expect("spawn vmux in pty");
    pty.expect(Regex("READY"))
        .expect("embedded tmux pane should be ready");

    pty.send("\u{1b}[<0;200;200M")
        .expect("send raw sgr mouse sequence to vmux host");
    std::thread::sleep(std::time::Duration::from_secs(2));

    let log = fs::read_to_string(&log_path).expect("read tmux invocation log");
    assert!(
        log.contains("MOUSE:1b5b3c303b3230303b3230304d"),
        "vmux should forward unclaimed sgr mouse bytes unchanged to tmux"
    );
}
