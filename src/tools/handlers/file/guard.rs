//! Workspace path guard for the file tools.
//!
//! Every mutating tool resolves its target through [`resolve_in_workspace`].
//! The model supplies the path, so it may be absolute, may contain `..`, may be
//! a UNC/device path, or may point at a symlink that leaves the repository. All
//! of those are refused here, before the filesystem is touched.

use std::path::{Component, Path, PathBuf};

/// Locations the agent must never read or write, even inside the workspace.
/// A prompt-injected model goes for credentials first.
const PROTECTED_FRAGMENTS: &[&str] = &[
  "\\appdata\\roaming\\zarnkina\\",
  "\\appdata\\local\\zarnkina\\",
  "\\.dsh\\",
  "\\.zarncode\\",
  "\\.ssh\\",
  "\\.gnupg\\",
  "\\.aws\\",
  "\\.config\\gcloud\\",
  "\\.docker\\config.json",
  "\\startup\\",
  "\\google\\chrome\\user data\\",
  "\\microsoft\\edge\\user data\\",
  "\\mozilla\\firefox\\profiles\\",
  "\\appdata\\local\\microsoft\\credentials\\",
  "\\appdata\\roaming\\microsoft\\credentials\\",
  "\\windows\\system32\\config\\",
  "\\windows\\system32\\winevt\\",
  "\\ntds.dit",
  "\\id_rsa",
  "\\id_ed25519",
  "\\.netrc",
  "\\.npmrc",
  "\\auth.json",
  "\\.credentials.yaml",
  "\\config.toml",
  "\\prompts\\system.md",
];

/// True when the path names a credential store, persistence location, or the
/// agent's own configuration.
pub fn is_protected(path: &Path) -> bool {
  let norm = canonical_loose(path)
    .to_string_lossy()
    .replace('/', "\\")
    .to_lowercase();
  PROTECTED_FRAGMENTS.iter().any(|f| norm.contains(f))
}

/// Canonicalize the deepest existing ancestor and re-attach the tail, so a file
/// that does not exist yet can still be compared against a root.
fn canonical_loose(path: &Path) -> PathBuf {
  let mut tail: Vec<std::ffi::OsString> = Vec::new();
  let mut cur = path.to_path_buf();
  loop {
    if let Ok(base) = cur.canonicalize() {
      let mut out = base;
      for part in tail.iter().rev() {
        out.push(part);
      }
      return out;
    }
    match (cur.file_name(), cur.parent()) {
      (Some(name), Some(parent)) => {
        tail.push(name.to_os_string());
        cur = parent.to_path_buf();
      }
      _ => return path.to_path_buf(),
    }
  }
}

fn starts_with_ci(path: &Path, root: &Path) -> bool {
  let norm = |p: &Path| {
    p.to_string_lossy()
      .replace('/', "\\")
      .trim_end_matches('\\')
      .to_lowercase()
  };
  let p = norm(path);
  let r = norm(root);
  p == r || p.starts_with(&(r + "\\"))
}

/// Reject device, UNC, and drive-relative forms before anything else looks at
/// them. Windows treats `\\?\`, `\\.\`, and `\\server\share` specially.
fn reject_exotic(raw: &str) -> Result<(), String> {
  let lowered = raw.to_lowercase();
  if lowered.starts_with("\\\\?\\") || lowered.starts_with("\\\\.\\") {
    return Err("device paths are not allowed".to_string());
  }
  if raw.starts_with("\\\\") || raw.starts_with("//") {
    return Err("UNC paths are not allowed".to_string());
  }
  if lowered.contains('\0') {
    return Err("NUL byte in path".to_string());
  }
  Ok(())
}

/// Resolve `raw` against `cwd` and prove the result stays inside `cwd`.
///
/// Returns the canonicalized target so the caller writes to the same path that
/// was validated.
pub fn resolve_in_workspace(cwd: &Path, raw: &str) -> Result<PathBuf, String> {
  let raw = raw.trim();
  if raw.is_empty() {
    return Err("empty path".to_string());
  }
  reject_exotic(raw)?;

  let requested = Path::new(raw);
  if requested.components().any(|c| matches!(c, Component::ParentDir)) {
    return Err(format!("`..` is not allowed in a path: `{raw}`"));
  }

  // An absolute path is accepted only if it already lives inside the workspace.
  // Refusing every absolute path outright broke callers that pass a resolved
  // project path; the containment check below is what actually keeps the write
  // inside the tree.
  let joined = if requested.is_absolute() {
    requested.to_path_buf()
  } else {
    cwd.join(requested)
  };
  let root = canonical_loose(cwd);
  let resolved = canonical_loose(&joined);

  if !starts_with_ci(&resolved, &root) {
    return Err(format!(
      "path escapes the workspace: `{raw}` resolves to {} which is outside {}",
      resolved.display(),
      root.display()
    ));
  }
  if is_protected(&resolved) {
    return Err(format!("path is a protected location: `{raw}`"));
  }
  Ok(resolved)
}

/// Same containment rules, but for read-only tools where the path must exist.
pub fn resolve_for_read(cwd: &Path, raw: &str) -> Result<PathBuf, String> {
  let resolved = resolve_in_workspace(cwd, raw)?;
  if resolved.exists() {
    // A symlink or junction can point anywhere: canonicalize again after the
    // file is known to exist and re-check the root.
    let real = resolved
      .canonicalize()
      .map_err(|e| format!("cannot resolve `{raw}`: {e}"))?;
    let root = canonical_loose(cwd);
    if !starts_with_ci(&real, &root) {
      return Err(format!("`{raw}` resolves outside the workspace via a link"));
    }
    if is_protected(&real) {
      return Err(format!("path is a protected location: `{raw}`"));
    }
    return Ok(real);
  }
  Err(format!("`{raw}` does not exist"))
}
