use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(task) = args.next() else {
        return Err("usage: cargo xtask ui [--debug]".into());
    };
    if task != "ui" {
        return Err(format!("unknown task {task:?}; available task: ui"));
    }

    let debug = match args.next() {
        None => false,
        Some(flag) if flag == "--debug" => true,
        Some(flag) => return Err(format!("unknown ui option {flag:?}")),
    };
    if let Some(extra) = args.next() {
        return Err(format!("unexpected argument {extra:?}"));
    }

    build_and_sync_ui(debug)
}

fn build_and_sync_ui(debug: bool) -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let ui = root.join("crates/lumiere-ui");
    let profile = if debug { "debug" } else { "release" };

    let mut command = Command::new("mise");
    command
        .args(["exec", "--", "dx", "build", "--platform", "web"])
        .current_dir(&ui);
    if !debug {
        command.arg("--release");
    }

    println!("Building Lumière UI ({profile}) in {}", ui.display());
    let status = command
        .status()
        .map_err(|error| format!("failed to start Dioxus build: {error}"))?;
    if !status.success() {
        return Err(format!("Dioxus build failed with {status}"));
    }

    let source = root
        .join("target/dx/lumiere-ui")
        .join(profile)
        .join("web/public");
    let destination = root.join("dist/web");
    sync_directory(&source, &destination)
        .map_err(|error| format!("failed to sync UI assets: {error}"))?;
    println!("Synced {} to {}", source.display(), destination.display());
    Ok(())
}

fn sync_directory(source: &Path, destination: &Path) -> io::Result<()> {
    if !source.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("build output {} does not exist", source.display()),
        ));
    }
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(destination)? {
        let entry = entry?;
        if entry.file_name() == ".gitkeep" {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    copy_directory(source, destination)
}

fn copy_directory(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&source_path, &destination_path)?;
        } else {
            fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}
