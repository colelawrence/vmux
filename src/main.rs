use std::path::PathBuf;

use vmux::{run, run_notify, RealTmuxAdapter, VmuxError};

enum Mode {
    Ui,
    Notify { payload_path: PathBuf },
}

fn parse_mode() -> Result<Mode, VmuxError> {
    let mut args = std::env::args_os().skip(1);
    let Some(first) = args.next() else {
        return Ok(Mode::Ui);
    };

    if first != "notify" {
        return Err(VmuxError::Usage(format!(
            "unrecognized argument: {}",
            first.to_string_lossy()
        )));
    }

    let mut payload_path: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--payload-path" {
            let Some(path) = args.next() else {
                return Err(VmuxError::Usage(
                    "notify usage: vmux notify [--payload-path] <payload-path>".to_string(),
                ));
            };
            if payload_path.replace(PathBuf::from(path)).is_some() {
                return Err(VmuxError::Usage(
                    "notify usage: vmux notify [--payload-path] <payload-path>".to_string(),
                ));
            }
            continue;
        }

        if payload_path.replace(PathBuf::from(arg)).is_some() {
            return Err(VmuxError::Usage(
                "notify usage: vmux notify [--payload-path] <payload-path>".to_string(),
            ));
        }
    }

    let Some(payload_path) = payload_path else {
        return Err(VmuxError::Terminal(
            "notify usage: vmux notify [--payload-path] <payload-path>".to_string(),
        ));
    };

    Ok(Mode::Notify { payload_path })
}

fn main() {
    let result = match parse_mode() {
        Ok(Mode::Ui) => {
            let mut adapter = RealTmuxAdapter::from_env();
            run(&mut adapter)
        }
        Ok(Mode::Notify { payload_path }) => run_notify(&payload_path),
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
