//! Portable, deliberately unsuccessful encoder child for host ownership tests.
//! `-version` only acknowledges this controlled process; no encode succeeds.
use std::{
    fs,
    path::PathBuf,
    time::{Duration, Instant},
};
fn main() {
    let executable = std::env::current_exe().unwrap();
    let root: PathBuf = executable.parent().unwrap().join("witness");
    fs::create_dir_all(&root).unwrap();
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["-version"] {
        println!("FreeShow controlled negative encoder fixture");
        return;
    }
    assert!(args.iter().any(|arg| arg == "-encoders"));
    fs::write(
        root.join(format!("listing-{}", std::process::id())),
        args.join("\n"),
    )
    .unwrap();
    if executable
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .ends_with("gate")
    {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !root.join("release").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    eprintln!("controlled listing rejected; no working encoder is claimed");
    std::process::exit(7);
}
