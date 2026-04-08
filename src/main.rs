use std::path::PathBuf;

use vmux::{run, run_clear, run_notify, RealTmuxAdapter, VmuxError};

enum Mode {
    Ui,
    Notify { payload_path: PathBuf },
    Clear { payload_path: PathBuf },
}

fn parse_mode() -> Result<Mode, VmuxError> {
    let mut args = std::env::args_os().skip(1);
    let Some(first) = args.next() else {
        return Ok(Mode::Ui);
    };

    let mode_name = if first == "notify" {
        "notify"
    } else if first == "clear" {
        "clear"
    } else {
        return Err(VmuxError::Usage(format!(
            "unrecognized argument: {}",
            first.to_string_lossy()
        )));
    };

    let usage = format!("{mode_name} usage: vmux {mode_name} [--payload-path] <payload-path>");
    let mut payload_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--payload-path" {
            let Some(path) = args.next() else {
                return Err(VmuxError::Usage(usage.clone()));
            };
            if payload_path.replace(PathBuf::from(path)).is_some() {
                return Err(VmuxError::Usage(usage.clone()));
            }
            continue;
        }

        if payload_path.replace(PathBuf::from(arg)).is_some() {
            return Err(VmuxError::Usage(usage.clone()));
        }
    }

    let Some(payload_path) = payload_path else {
        return Err(VmuxError::Usage(usage));
    };

    Ok(match mode_name {
        "notify" => Mode::Notify { payload_path },
        "clear" => Mode::Clear { payload_path },
        _ => unreachable!("validated mode"),
    })
}

fn main() {
    let result = match parse_mode() {
        Ok(Mode::Ui) => {
            let mut adapter = RealTmuxAdapter::from_env();
            run(&mut adapter)
        }
        Ok(Mode::Notify { payload_path }) => run_notify(&payload_path),
        Ok(Mode::Clear { payload_path }) => run_clear(&payload_path),
        Err(err) => Err(err),
    };

    if let Err(err) = result {
        eprintln!("vmux error: {err}");
        let code = if matches!(err, VmuxError::Usage(_)) {
            2
        } else {
            1
        };
        std::process::exit(code);
    }
}
