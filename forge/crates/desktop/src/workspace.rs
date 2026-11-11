use forge_domain::{FileEntry, WorkspaceView};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    process::Stdio,
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Duration, timeout},
};

const FILE_LIMIT: usize = 256 * 1024;
const GIT_LIMIT: usize = 192 * 1024;

pub fn guarded_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = Path::new(relative);
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("Choose a file inside this workspace.".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "This workspace is no longer available.")?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|_| "The selected file is no longer available.")?;
    if !path.starts_with(&root) {
        return Err("This link points outside the selected workspace.".into());
    }
    Ok(path)
}

pub fn read_file(root: &Path, relative: &str) -> Result<(String, bool), String> {
    let path = guarded_path(root, relative)?;
    if !path.is_file() {
        return Err("Select a regular file to preview.".into());
    }
    let file = std::fs::File::open(path).map_err(|e| format!("Cannot open file: {e}"))?;
    let mut bytes = Vec::new();
    file.take((FILE_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let truncated = bytes.len() > FILE_LIMIT;
    bytes.truncate(FILE_LIMIT);
    if bytes.contains(&0) {
        return Err(
            "This is a binary file. Document and media previews arrive in a later build.".into(),
        );
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

pub fn list_files(root: &Path, relative: &str) -> Result<(Vec<FileEntry>, bool), String> {
    let path = guarded_path(root, relative)?;
    if !path.is_dir() {
        return Err("Choose a workspace folder.".into());
    }
    let canonical_root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(path).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if [".git", "node_modules", "target"].contains(&name.as_str()) {
            continue;
        }
        if entries.len() == 300 {
            truncated = true;
            break;
        }
        let metadata = match entry.path().canonicalize() {
            Ok(p) if p.starts_with(&canonical_root) => p.metadata().ok(),
            _ => None,
        };
        if let Some(metadata) = metadata {
            let relative_path = entry
                .path()
                .strip_prefix(&canonical_root)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .into_owned();
            entries.push(FileEntry {
                path: relative_path,
                name,
                is_dir: metadata.is_dir(),
                size: metadata.len(),
            });
        }
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((entries, truncated))
}

async fn git(root: &Path, args: &[&str]) -> Result<(String, bool), String> {
    let mut child = Command::new("git")
        .args([
            "--no-optional-locks",
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "Git is unavailable on this computer.")?;
    let mut reader = child
        .stdout
        .take()
        .ok_or("Git output is unavailable.")?
        .take((GIT_LIMIT + 1) as u64);
    let mut bytes = Vec::new();
    let result = timeout(Duration::from_secs(8), reader.read_to_end(&mut bytes)).await;
    match result {
        Err(_) => {
            let _ = child.kill().await;
            return Err("Git took too long. Try a smaller workspace.".into());
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            return Err(e.to_string());
        }
        _ => {}
    }
    let truncated = bytes.len() > GIT_LIMIT;
    if truncated {
        let _ = child.kill().await;
        bytes.truncate(GIT_LIMIT);
    } else {
        let status = timeout(Duration::from_secs(2), child.wait())
            .await
            .map_err(|_| "Git did not finish.")?
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("Git information is unavailable for this workspace.".into());
        }
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}

pub async fn inspect(root: PathBuf, directory: String) -> Result<WorkspaceView, String> {
    let files_root = root.clone();
    let (files, file_limit) =
        tokio::task::spawn_blocking(move || list_files(&files_root, &directory))
            .await
            .map_err(|e| e.to_string())??;
    let (branch_result, status_result, diff_result) = tokio::join!(
        git(&root, &["branch", "--show-current"]),
        git(&root, &["status", "--short", "--untracked-files=normal"]),
        git(
            &root,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "HEAD",
                "--"
            ]
        )
    );
    let branch = branch_result
        .ok()
        .map(|r| r.0.trim().to_string())
        .filter(|s| !s.is_empty());
    let (status, status_limit) = status_result.unwrap_or_else(|e| (e, false));
    let (diff, diff_limit) = match diff_result {
        Ok(v) => v,
        Err(_) => git(
            &root,
            &["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--"],
        )
        .await
        .unwrap_or_else(|e| (e, false)),
    };
    Ok(WorkspaceView {
        branch,
        status,
        diff,
        files,
        truncated: file_limit || status_limit || diff_limit,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_absolute_and_symlink_escape() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("inside.txt"), "safe").unwrap();
        assert!(guarded_path(root.path(), "../outside").is_err());
        assert!(guarded_path(root.path(), "/etc/passwd").is_err());
        assert_eq!(read_file(root.path(), "inside.txt").unwrap().0, "safe");
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            std::fs::write(outside.path().join("private"), "private").unwrap();
            std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
            assert!(read_file(root.path(), "link/private").is_err());
        }
    }

    #[test]
    fn large_and_binary_previews_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("large"), "x".repeat(FILE_LIMIT + 100)).unwrap();
        let (text, truncated) = read_file(root.path(), "large").unwrap();
        assert_eq!(text.len(), FILE_LIMIT);
        assert!(truncated);
        std::fs::write(root.path().join("binary"), [0u8, 1, 2]).unwrap();
        assert!(read_file(root.path(), "binary").is_err());
    }
}
