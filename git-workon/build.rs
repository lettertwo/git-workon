use std::{env::var_os, fs::write, io, process::Command};

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

    let readme_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../README.md");
    println!("cargo:rerun-if-changed={}", readme_path.display());

    generate_manpages()?;
    generate_completions()?;
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

/// Parse a markdown document into a map of section heading -> section body.
/// Sections are delimited by `## Heading` lines.
fn parse_readme_sections(readme: &str) -> std::collections::HashMap<String, String> {
    let mut sections = std::collections::HashMap::new();
    for part in readme.split("\n## ").skip(1) {
        if let Some((heading, content)) = part.split_once('\n') {
            sections.insert(heading.trim().to_string(), content.to_string());
        }
    }
    sections
}

/// Apply inline formatting to a line of markdown text:
/// - `code` -> \fBcode\fR
/// - [text](url) -> text
fn format_inline(s: &str) -> String {
    let mut result = String::new();
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '`' => {
                let mut code = String::new();
                for nc in chars.by_ref() {
                    if nc == '`' {
                        break;
                    }
                    code.push(nc);
                }
                let escaped = code.replace('\\', "\\\\");
                result.push_str(&format!("\\fB{escaped}\\fR"));
            }
            '[' => {
                let mut text = String::new();
                let mut closed = false;
                for nc in chars.by_ref() {
                    if nc == ']' {
                        closed = true;
                        break;
                    }
                    text.push(nc);
                }
                if closed && chars.peek() == Some(&'(') {
                    chars.next(); // consume '('
                    for nc in chars.by_ref() {
                        if nc == ')' {
                            break;
                        }
                    }
                    result.push_str(&text);
                } else {
                    result.push('[');
                    result.push_str(&text);
                    if closed {
                        result.push(']');
                    }
                }
            }
            _ => result.push(c),
        }
    }

    result
}

/// Convert a README section body (markdown) to roff text.
/// Handles: ### subheadings, fenced code blocks, inline `code`, [text](url), empty lines.
fn markdown_to_roff_string(content: &str) -> String {
    let mut roff = String::new();
    let mut in_code_block = false;
    let mut need_pp = false;

    for line in content.lines() {
        if in_code_block {
            if line.starts_with("```") {
                roff.push_str(".EE\n.RE\n");
                in_code_block = false;
                need_pp = true;
            } else {
                let escaped = line.replace('\\', "\\\\");
                if escaped.starts_with('.') {
                    roff.push_str("\\&");
                }
                roff.push_str(&escaped);
                roff.push('\n');
            }
        } else if line.starts_with("```") {
            roff.push_str(".RS 4\n.EX\n");
            in_code_block = true;
            need_pp = false;
        } else if let Some(heading) = line.strip_prefix("### ") {
            roff.push_str(&format!(".SS \"{heading}\"\n"));
            need_pp = false;
        } else if line.is_empty() {
            if !need_pp && !roff.is_empty() {
                need_pp = true;
            }
        } else {
            if need_pp {
                roff.push_str(".PP\n");
                need_pp = false;
            }
            let formatted = format_inline(line);
            if formatted.starts_with('.') {
                roff.push_str("\\&");
            }
            roff.push_str(&formatted);
            roff.push('\n');
        }
    }

    roff
}

fn generate_manpages() -> io::Result<()> {
    use clap::CommandFactory;
    use clap_mangen::Man;

    use crate::cli::Cli;

    let cmd = Cli::command();
    let man = Man::new(cmd);

    // Render the base man page
    let mut buffer: Vec<u8> = Default::default();
    man.render(&mut buffer)?;
    let mut man_content = String::from_utf8(buffer).expect("man page is valid UTF-8");

    // Try to inject README sections (gracefully skip if README is missing, e.g. on crates.io)
    let readme_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../README.md");
    if readme_path.exists() {
        let readme = std::fs::read_to_string(&readme_path)?;
        let raw_sections = parse_readme_sections(&readme);

        let mut custom = String::new();
        for (readme_heading, man_heading) in &[
            ("Quick start", "EXAMPLES"),
            ("Shell integration", "SHELL INTEGRATION"),
            ("Configuration", "CONFIGURATION"),
        ] {
            if let Some(content) = raw_sections.get(*readme_heading) {
                let roff_content = markdown_to_roff_string(content);
                custom.push_str(&format!(".SH \"{man_heading}\"\n{roff_content}"));
            }
        }

        if !custom.is_empty() {
            const VERSION_SECTION: &str = ".SH VERSION\n";
            if let Some(pos) = man_content.find(VERSION_SECTION) {
                man_content.insert_str(pos, &custom);
            } else {
                man_content.push_str(&custom);
            }
        }
    }

    // Write to OUT_DIR so the binary can embed it via include_str!
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo");
    let out_path = format!("{}/git-workon.1", out_dir);
    write(&out_path, man_content.as_bytes())?;

    // Also write to dist/ for packaging (cargo-dist include).
    write_if_changed("dist/git-workon.1", man_content.as_bytes())?;

    Ok(())
}

/// Write `content` to `<CARGO_MANIFEST_DIR>/<rel_path>`, but only if the file
/// does not already contain identical bytes. This prevents Cargo from seeing a
/// mtime change and triggering an unnecessary rebuild loop.
fn write_if_changed(rel_path: &str, content: &[u8]) -> io::Result<()> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = dir.join(rel_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let existing = std::fs::read(&path)?;
        if existing == content {
            return Ok(());
        }
    }
    write(&path, content)
}

fn generate_completions() -> io::Result<()> {
    use clap::CommandFactory;
    use clap_complete::env::{Bash, EnvCompleter, Fish, Zsh};

    use crate::cli::Cli;

    let pkg_name = env!("CARGO_PKG_NAME");
    let cmd = Cli::command();

    let shells: &[(&dyn EnvCompleter, &str)] = &[
        (&Bash, "dist/git-workon.bash"),
        (&Zsh, "dist/_git-workon"),
        (&Fish, "dist/git-workon.fish"),
    ];

    for (shell, rel_path) in shells {
        let mut buf: Vec<u8> = Vec::new();
        shell
            .write_registration("COMPLETE", pkg_name, pkg_name, pkg_name, &mut buf)
            .map_err(io::Error::other)?;
        write_if_changed(rel_path, &buf)?;
    }

    // cmd is only needed to satisfy CommandFactory; keep it alive to the end.
    drop(cmd);

    Ok(())
}
