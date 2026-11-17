use amaterasu_stream_failure_audit::{report, scan};
use std::{
    env, fs,
    io::{self, Write},
    path::PathBuf,
    process::{Command, ExitCode},
    sync::atomic::{AtomicU64, Ordering},
};

fn main() -> ExitCode {
    match run() {
        Ok(complete) => {
            if complete {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            }
        }
        Err(error) => {
            eprintln!("stream-failure-audit: {error}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, String> {
    let mut root = env::current_dir().map_err(|e| e.to_string())?;
    let mut json = None;
    let mut markdown = None;
    let mut symbols = Vec::new();
    let mut depth = 5;
    let mut paths = 3;
    let mut markdown_limit = 200;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!(
                "Usage: audit_stream_failures.py --root REPOSITORY --json REPORT.json [--markdown REPORT.md] [--decision-symbol package::Type::method] [--max-depth 5] [--max-paths 3] [--markdown-limit 200]\n\nRead-only source analysis. Findings never cause failure; exit 2 means invalid options, unmatched policy symbols, or incomplete source parsing/I/O. Missing submodules/symlinks remain explicit coverage gaps in an otherwise successful inventory."
            );
            return Ok(true);
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--root" => root = PathBuf::from(value),
            "--json" => json = Some(PathBuf::from(value)),
            "--markdown" => markdown = Some(PathBuf::from(value)),
            "--decision-symbol" => symbols.push(value),
            "--max-depth" => depth = bounded_number(&arg, &value, 1, 12)?,
            "--max-paths" => paths = bounded_number(&arg, &value, 1, 20)?,
            "--markdown-limit" => markdown_limit = bounded_number(&arg, &value, 0, 100_000)?,
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    let json = json.ok_or("--json is required; choose an output path outside source files")?;
    root = root.canonicalize().map_err(|e| e.to_string())?;
    let json = output_path(&root, json)?;
    let markdown = markdown.map(|p| output_path(&root, p)).transpose()?;
    if markdown.as_ref() == Some(&json) {
        return Err("JSON and Markdown outputs must differ".into());
    }
    let source = serde_json::json!({
        "root":root,
        "git_head_before":git(&root, &["rev-parse", "HEAD"]),
        "git_status_before":git(&root, &["status", "--porcelain", "--untracked-files=no", "--ignore-submodules=all"]),
        "submodules_before":git(&root, &["submodule", "status", "--recursive"]),
        "path_depth_limit":depth,"path_count_limit":paths,
        "scope":["crates/", "modules/"]
    });
    let inventory = scan::scan(&root);
    let mut result = report::build(inventory, source, symbols, depth, paths);
    result.source["git_head_after"] = git(&root, &["rev-parse", "HEAD"]).into();
    result.source["git_status_after"] = git(
        &root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=no",
            "--ignore-submodules=all",
        ],
    )
    .into();
    result.source["provenance_changed_during_scan"] = (result.source["git_head_before"]
        != result.source["git_head_after"]
        || result.source["git_status_before"] != result.source["git_status_after"])
        .into();
    let complete =
        result.inventory.errors.is_empty() && result.unmatched_decision_symbols.is_empty();
    let bytes = serde_json::to_vec_pretty(&result).map_err(|e| e.to_string())?;
    write_artifact(&json, &bytes).map_err(|e| format!("{}: {e}", json.display()))?;
    if let Some(path) = markdown {
        write_artifact(&path, report::markdown(&result, markdown_limit).as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    println!(
        "{} files, {} functions, {} observations, {} parse/I/O errors, {} coverage gaps; {}",
        result.inventory.files.len(),
        result.inventory.functions.len(),
        result.findings.len(),
        result.inventory.errors.len(),
        result.inventory.gaps.len(),
        json.display()
    );
    Ok(complete)
}

/// Replace an artifact rather than truncating an existing inode that may also
/// name a source file. create_new never touches an existing temporary path.
fn write_artifact(path: &std::path::Path, bytes: &[u8]) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("output has no parent"))?;
    let temporary = parent.join(format!(
        ".stream-audit-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.flush()?;
        if let Ok(metadata) = fs::metadata(path) {
            fs::set_permissions(&temporary, metadata.permissions())?;
        }
        Ok::<(), io::Error>(())
    })();
    drop(file);
    let result = result.and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn output_path(root: &std::path::Path, path: PathBuf) -> Result<PathBuf, String> {
    let path = if path.is_absolute() {
        path
    } else {
        env::current_dir().map_err(|e| e.to_string())?.join(path)
    };
    if path.is_symlink() {
        return Err(format!("refusing symlink output: {}", path.display()));
    }
    let parent = path
        .parent()
        .ok_or("output has no parent")?
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let path = parent.join(path.file_name().ok_or("output has no file name")?);
    if ["crates", "modules", ".git"]
        .iter()
        .any(|p| path.starts_with(root.join(p)))
        || path
            .extension()
            .is_some_and(|e| e == "rs" || e == "toml" || e == "lock" || e == "py")
    {
        return Err(format!(
            "refusing to write an audit into source/manifest inputs: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn bounded_number(arg: &str, value: &str, min: usize, max: usize) -> Result<usize, String> {
    let number = value
        .parse::<usize>()
        .map_err(|_| format!("invalid {arg}: {value}"))?;
    if (min..=max).contains(&number) {
        Ok(number)
    } else {
        Err(format!("{arg} must be between {min} and {max}"))
    }
}

fn git(root: &std::path::Path, args: &[&str]) -> String {
    match Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
    {
        Ok(result) if result.status.success() => {
            String::from_utf8_lossy(&result.stdout).trim().into()
        }
        Ok(result) => format!(
            "unavailable: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ),
        Err(error) => format!("unavailable: {error}"),
    }
}
