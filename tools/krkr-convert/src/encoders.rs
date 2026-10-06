//! External encoders live beside the converter, independently of its cwd.
use crate::media::Result;
use std::path::{Path, PathBuf};

pub fn audio() -> Result<PathBuf> {
    sibling("at9tool")
}

fn sibling(name: &str) -> Result<PathBuf> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    beside(&executable, name)
}

fn beside(executable: &Path, name: &str) -> Result<PathBuf> {
    let tool = location(executable, name)?;
    required(&tool)?;
    Ok(tool)
}

fn location(executable: &Path, name: &str) -> Result<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    Ok(executable
        .parent()
        .ok_or("converter directory unavailable")?
        .join(&file))
}

pub(crate) fn required(tool: &Path) -> Result<()> {
    if !tool.is_file() {
        return Err(format!(
            "缺少 {}：请将它放在 krkr-convert 同一目录（{}）",
            tool.file_name().unwrap_or_default().to_string_lossy(),
            tool.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/internal/encoders.rs"]
mod tests;
