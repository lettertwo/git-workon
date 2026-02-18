use std::{env::var_os, fs::create_dir_all, fs::write, io, process::Command};

#[path = "src/cli.rs"]
mod cli;

fn main() -> std::io::Result<()> {
    let pkg_version = env!("CARGO_PKG_VERSION");
    let version = match var_os("PROFILE") {
        Some(profile) if profile == "release" => format!("v{pkg_version}"),
        _ => git_version().unwrap_or_else(|| format!("v{pkg_version}-unknown")),
    };
    println!("cargo:rustc-env=GIT_WORKON_VERSION={version}");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-changed=templates/");
    println!("cargo:rerun-if-changed=tests/");

    generate_manpages()?;
    Ok(())
}

fn git_version() -> Option<String> {
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut git = Command::new("git");
    git.args([
        "-C",
        dir,
        "describe",
        "--tags",
        "--match=v*.*.*",
        "--always",
        "--broken",
    ]);

    let output = git.output().ok()?;
    if !output.status.success() || output.stdout.is_empty() || !output.stderr.is_empty() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn generate_manpages() -> io::Result<()> {
    use clap::CommandFactory;
    use clap_mangen::Man;

    use crate::cli::Cli;

    let dir = "man";
    let path = format!("{}/{}.1", dir, env!("CARGO_PKG_NAME"));
    let cmd = Cli::command();
    let man = Man::new(cmd);
    let mut buffer: Vec<u8> = Default::default();
    man.render(&mut buffer)?;

    create_dir_all(dir)?;
    write(&path, buffer)?;

    println!("cargo:warning=generated manpage: {:?}", &path);

    Ok(())
}
