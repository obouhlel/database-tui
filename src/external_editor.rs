//! Opens a file in the user's editor (`$VISUAL`, `$EDITOR`, else nano or
//! vi), on a given line, and waits for it to exit.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

/// The editor command line, as the user configured it (may hold arguments,
/// e.g. `code --wait`).
pub fn command() -> String {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var)
            && !v.trim().is_empty()
        {
            return v;
        }
    }
    for candidate in ["nano", "vi"] {
        if which(candidate) {
            return candidate.into();
        }
    }
    "vi".into()
}

fn which(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// Arguments opening `path` at `line` for this editor (most take `+line`,
/// a few `path:line`).
pub fn line_args(editor: &str, path: &str, line: usize) -> Vec<String> {
    let program = editor
        .split_whitespace()
        .next()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("");
    match program {
        "code" | "codium" | "code-insiders" | "cursor" => {
            vec!["-g".into(), format!("{path}:{line}")]
        }
        "hx" | "helix" | "subl" | "zed" => vec![format!("{path}:{line}")],
        _ => vec![format!("+{line}"), path.to_string()],
    }
}

/// Runs the editor on `path` at `line` and waits for it.
pub fn open(path: &Path, line: usize) -> Result<()> {
    let editor = command();
    let args = line_args(&editor, &path.to_string_lossy(), line);
    // Through the shell: `$EDITOR` may contain arguments (`code --wait`).
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg("sh")
        .args(&args)
        .status()
        .with_context(|| format!("impossible de lancer l'éditeur `{editor}`"))?;
    if !status.success() {
        bail!("l'éditeur `{editor}` s'est terminé en erreur ({status})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_arguments_per_editor() {
        assert_eq!(line_args("vim", "/c.toml", 12), ["+12", "/c.toml"]);
        assert_eq!(line_args("/usr/bin/nano", "/c.toml", 3), ["+3", "/c.toml"]);
        assert_eq!(line_args("code --wait", "/c.toml", 7), ["-g", "/c.toml:7"]);
        assert_eq!(line_args("hx", "/c.toml", 7), ["/c.toml:7"]);
    }
}
