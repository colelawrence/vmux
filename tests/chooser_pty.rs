use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use expectrl::session::Session;
use expectrl::{Eof, Error, Regex};
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
        "#!/bin/sh\nset -eu\n\nLOG=\"{}\"\n\necho \"tmux invoked: $0 $@\" >> \"$LOG\"\ncase \"$1\" in\n  list-sessions)\n    echo 's1:1:0'\n    echo 's2:1:0'\n    ;;\n  attach-session)\n    printf 'TMUX_SESSION:s1\\n'\n    sleep 2\n    ;;\n  *)\n    echo \"unexpected tmux command: $0 $@\" >> \"$LOG\"\n    ;;\n esac\n",
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

    pty.expect(Regex("sessions"))
        .expect("sidebar should render");
    pty.expect(Regex("TMUX_SESSION:s1"))
        .expect("embedded tmux pane should render");

    pty.send("q").expect("send quit");
    pty.expect(Eof).expect("vmux should exit cleanly");
}
