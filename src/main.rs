use vmux::{run, RealTmuxAdapter};

fn main() {
    let mut adapter = RealTmuxAdapter::from_env();
    if let Err(err) = run(&mut adapter) {
        eprintln!("vmux error: {err}");
        std::process::exit(1);
    }
}
