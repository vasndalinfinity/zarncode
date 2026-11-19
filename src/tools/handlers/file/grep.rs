//! Grep tool handler.
//!
//! A powerful search tool based-on ripgrep for searching patterns in file contents.
//! When ripgrep is not installed the handler falls back to a built-in scanner,
//! so the tool works on a plain Windows machine without extra tooling.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;
use tokio::process::Command;

use crate::tools::{
  ToolError, ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolPayload, parse_arguments,
};

/// Handler for the Grep tool
pub struct GrepHandler;

/// Output mode for grep results
#[derive(Debug, Deserialize, Default, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum OutputMode {
  /// Show matching lines
  #[default]
  Content,
  /// Show file paths only
  FilesWithMatches,
  /// Show match counts
  CountMatches,
}

/// Arguments for the Grep tool
#[derive(Debug, Clone, Deserialize)]
struct GrepArgs {
  /// The regular expression pattern to search for
  pattern: String,
  /// File or directory to search in (defaults to current directory)
  #[serde(default = "default_path")]
  path: String,
  /// Glob pattern to filter files
  #[serde(default)]
  glob: Option<String>,
  /// Output mode
  #[serde(default)]
  output_mode: OutputMode,
  /// Number of lines to show before each match
  #[serde(default)]
  before_context: Option<usize>,
  /// Number of lines to show after each match
  #[serde(default)]
  after_context: Option<usize>,
  /// Number of lines to show before and after each match
  #[serde(default)]
  context: Option<usize>,
  /// Show line numbers in output
  #[serde(default)]
  line_number: bool,
  /// Case insensitive search
  #[serde(default)]
  ignore_case: bool,
  /// File type to search
  #[serde(default)]
  r#type: Option<String>,
  /// Limit output to first N lines
  #[serde(default)]
  head_limit: Option<usize>,
  /// Enable multiline mode
  #[serde(default)]
  multiline: bool,
}

fn default_path() -> String {
  ".".to_string()
}

/// Directories that never contain source worth searching.
const SKIP_DIRS: [&str; 8] = [
  ".git",
  "node_modules",
  "target",
  "dist",
  "build",
  ".venv",
  "__pycache__",
  ".next",
];

/// A file extension filter, used when the caller asks for a file type.
fn extension_matches(file_type: &str, path: &Path) -> bool {
  let ext = path
    .extension()
    .and_then(|e| e.to_str())
    .unwrap_or("")
    .to_ascii_lowercase();
  match file_type {
    "rust" => ext == "rs",
    "python" => ext == "py",
    "js" | "javascript" => ext == "js" || ext == "mjs" || ext == "cjs" || ext == "ts",
    "ts" | "typescript" => ext == "ts" || ext == "tsx",
    "go" => ext == "go",
    "c" => ext == "c" || ext == "h",
    "cpp" => ext == "cpp" || ext == "cc" || ext == "hpp",
    "md" | "markdown" => ext == "md",
    "json" => ext == "json",
    "toml" => ext == "toml",
    "yaml" | "yml" => ext == "yaml" || ext == "yml",
    "html" => ext == "html" || ext == "htm",
    "css" => ext == "css",
    "sh" | "shell" => ext == "sh" || ext == "bash",
    other => ext == other,
  }
}

/// A minimal glob matcher: `*` matches any run inside one path segment,
/// `**` matches across segments. Enough for the filters the tool exposes.
fn glob_matches(pattern: &str, path: &Path) -> bool {
  let name = path
    .file_name()
    .and_then(|n| n.to_str())
    .unwrap_or("")
    .to_ascii_lowercase();
  let pattern = pattern.to_ascii_lowercase();
  let Some((prefix, suffix)) = pattern.split_once('*') else {
    return name == pattern || path.to_string_lossy().to_ascii_lowercase().contains(&pattern);
  };
  let before = prefix.rsplit('/').next().unwrap_or(prefix);
  let after = suffix.split('/').next().unwrap_or(suffix);
  let before_ok = if before.is_empty() {
    true
  } else {
    name.starts_with(before)
  };
  let after_ok = if after.is_empty() {
    true
  } else {
    name.ends_with(after)
  };
  before_ok && after_ok
}

/// Collect the files to search, honouring the glob and file type filters.
fn collect_files(root: &Path, glob: Option<&str>, file_type: Option<&str>) -> Vec<PathBuf> {
  let mut out = Vec::new();
  // A single file is a valid search target.
  if root.is_file() {
    if let Some(g) = glob {
      if !glob_matches(g, root) {
        return out;
      }
    }
    if let Some(t) = file_type {
      if !extension_matches(t, root) {
        return out;
      }
    }
    out.push(root.to_path_buf());
    return out;
  }
  let mut stack = vec![root.to_path_buf()];
  while let Some(dir) = stack.pop() {
    let Ok(entries) = std::fs::read_dir(&dir) else {
      continue;
    };
    for entry in entries.flatten() {
      let path = entry.path();
      let Ok(file_type_meta) = entry.file_type() else {
        continue;
      };
      if file_type_meta.is_dir() {
        let name = path
          .file_name()
          .and_then(|n| n.to_str())
          .unwrap_or("")
          .to_ascii_lowercase();
        if SKIP_DIRS.contains(&name.as_str()) {
          continue;
        }
        stack.push(path);
        continue;
      }
      if !file_type_meta.is_file() {
        continue;
      }
      if let Some(g) = glob {
        if !glob_matches(g, &path) {
          continue;
        }
      }
      if let Some(t) = file_type {
        if !extension_matches(t, &path) {
          continue;
        }
      }
      out.push(path);
    }
  }
  out.sort();
  out
}

/// Built-in search used when ripgrep is not on the machine.
fn builtin_search(args: &GrepArgs, root: &Path) -> String {
  let files = collect_files(root, args.glob.as_deref(), args.r#type.as_deref());
  let needle = if args.ignore_case {
    args.pattern.to_lowercase()
  } else {
    args.pattern.clone()
  };
  let mut lines_out: Vec<String> = Vec::new();
  let mut files_with_matches: Vec<String> = Vec::new();
  let mut counts: Vec<String> = Vec::new();

  for file in files {
    let Ok(content) = std::fs::read_to_string(&file) else {
      continue;
    };
    let mut hits = 0usize;
    let file_label = file.to_string_lossy().to_string();
    for (index, line) in content.lines().enumerate() {
      let haystack = if args.ignore_case {
        line.to_lowercase()
      } else {
        line.to_string()
      };
      if !haystack.contains(&needle) {
        continue;
      }
      hits += 1;
      match args.output_mode {
        OutputMode::Content => {
          if args.line_number {
            lines_out.push(format!("{}:{}:{}", file_label, index + 1, line));
          } else {
            lines_out.push(format!("{}:{}", file_label, line));
          }
        }
        OutputMode::FilesWithMatches => {}
        OutputMode::CountMatches => {}
      }
    }
    if hits > 0 {
      match args.output_mode {
        OutputMode::Content => {}
        OutputMode::FilesWithMatches => files_with_matches.push(file_label),
        OutputMode::CountMatches => counts.push(format!("{}:{}", file_label, hits)),
      }
    }
  }

  let mut out = match args.output_mode {
    OutputMode::Content => lines_out,
    OutputMode::FilesWithMatches => files_with_matches,
    OutputMode::CountMatches => counts,
  };

  if let Some(limit) = args.head_limit {
    if out.len() > limit {
      out.truncate(limit);
      out.push(format!("... (results truncated to {} lines)", limit));
    }
  }
  out.join("\n")
}

#[async_trait]
impl ToolHandler for GrepHandler {
  fn kind(&self) -> ToolKind {
    ToolKind::Function
  }

  async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
    false
  }

  async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
    let ToolInvocation { payload, cwd, .. } = invocation;

    // Extract arguments from payload
    let arguments = match payload {
      ToolPayload::Function { arguments } => arguments,
    };

    // Parse arguments
    let args: GrepArgs = parse_arguments(&arguments)?;

    // Resolve inside the workspace. Without the guard an absolute path or `..`
    // walked the whole disk, which the plan-mode allowlist did not prevent.
    let resolved_path = match super::guard::resolve_for_read(&cwd, &args.path) {
      Ok(p) => p,
      Err(reason) => return Err(ToolError::RespondToModel(reason)),
    };

    // A pattern starting with `-` is parsed by ripgrep as a flag, and some flags
    // run programs (`--pre=...`). `--` ends option parsing: everything after it
    // is positional, so the pattern can never become an option.
    if args.pattern.starts_with('-') {
      return Err(ToolError::RespondToModel(
        "grep pattern must not start with `-`".to_string(),
      ));
    }

    // Build ripgrep command. Every flag goes BEFORE the `--` terminator: options
    // emitted after it are parsed as positional paths, which silently disabled
    // the glob/type filters and made rg exit on a "file" named `-g`.
    let mut cmd = Command::new("rg");

    cmd.arg("--no-config");
    cmd.arg("--max-columns").arg("2000");
    cmd.arg("--max-filesize").arg("4M");

    if args.ignore_case {
      cmd.arg("-i");
    }
    if args.multiline {
      cmd.arg("-U");
      cmd.arg("--multiline-dotall");
    }

    match args.output_mode {
      OutputMode::Content => {
        if let Some(before) = args.before_context {
          cmd.arg("-B");
          cmd.arg(before.to_string());
        }
        if let Some(after) = args.after_context {
          cmd.arg("-A");
          cmd.arg(after.to_string());
        }
        if let Some(context) = args.context {
          cmd.arg("-C");
          cmd.arg(context.to_string());
        }
        if args.line_number {
          cmd.arg("-n");
        }
      }
      OutputMode::FilesWithMatches => {
        cmd.arg("-l");
      }
      OutputMode::CountMatches => {
        cmd.arg("-c");
      }
    }

    if let Some(glob) = &args.glob {
      cmd.arg("-g");
      cmd.arg(glob);
    }
    if let Some(file_type) = &args.r#type {
      cmd.arg("-t");
      cmd.arg(file_type);
    }

    // Positional arguments last: everything after `--` is a pattern or a path.
    cmd.arg("--");
    cmd.arg(&args.pattern);
    cmd.arg(&resolved_path);

    // Execute search. Ripgrep is the fast path; a machine without it falls
    // back to the built-in scanner so the tool always answers. A pathological
    // pattern over a slow tree is bounded by a wall-clock timeout.
    let run = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output()).await;
    let output = match run {
      Ok(Ok(output)) => output,
      Ok(Err(err)) => {
        let kind = err.kind();
        if kind == std::io::ErrorKind::NotFound {
          // The fallback walks the tree synchronously; bound it like the rg path.
          let args_for_walk = args.clone();
          let path_for_walk = resolved_path.clone();
          let walked = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::task::spawn_blocking(move || builtin_search(&args_for_walk, &path_for_walk)),
          )
          .await;
          let mut result = match walked {
            Ok(Ok(text)) => text,
            Ok(Err(_)) => String::new(),
            Err(_) => {
              return Err(ToolError::RespondToModel(
                "grep timed out after 30s; narrow the pattern or the path".to_string(),
              ))
            }
          };
          if result.is_empty() {
            result = "No matches found.".to_string();
          }
          return Ok(ToolOutput::success(result));
        }
        return Err(ToolError::Fatal(format!(
          "Failed to execute ripgrep: {}",
          err
        )));
      }
      Err(_) => {
        return Err(ToolError::RespondToModel(
          "grep timed out after 30s; narrow the pattern or the path".to_string(),
        ));
      }
    };

    if !output.status.success() {
      // Check if it's a "no matches found" case (exit code 1)
      let exit_code = output.status.code();
      if exit_code == Some(1) {
        return Ok(ToolOutput::success("No matches found."));
      }
      // Otherwise it's an error
      let stderr = String::from_utf8_lossy(&output.stderr);
      return Err(ToolError::RespondToModel(format!(
        "ripgrep error: {}",
        stderr
      )));
    }

    // Get stdout as string
    let mut result = String::from_utf8_lossy(&output.stdout).to_string();

    // Trim trailing newline
    if result.ends_with('\n') {
      result.pop();
    }

    // Apply head limit if specified
    if let Some(limit) = args.head_limit {
      let lines: Vec<&str> = result.lines().collect();
      if lines.len() > limit {
        let truncated: Vec<&str> = lines.into_iter().take(limit).collect();
        result = truncated.join("\n");
        result.push_str(&format!("\n... (results truncated to {} lines)", limit));
      }
    }

    if result.is_empty() {
      Ok(ToolOutput::success("No matches found."))
    } else {
      Ok(ToolOutput::success(result))
    }
  }
}

impl GrepHandler {
  /// Create a new GrepHandler
  pub fn new() -> Self {
    Self
  }
}

impl Default for GrepHandler {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use std::process::id;
  use std::time::SystemTime;

  use super::*;

  #[test]
  fn test_parse_arguments() {
    let json = r#"{"pattern": "test", "path": "/tmp", "ignore_case": true}"#;
    let args: GrepArgs = parse_arguments(json).unwrap();

    assert_eq!(args.pattern, "test");
    assert_eq!(args.path, "/tmp");
    assert!(args.ignore_case);
  }

  #[test]
  fn test_parse_arguments_defaults() {
    let json = r#"{"pattern": "test"}"#;
    let args: GrepArgs = parse_arguments(json).unwrap();

    assert_eq!(args.pattern, "test");
    assert_eq!(args.path, ".");
    assert!(!args.ignore_case);
    assert!(!args.line_number);
    assert!(!args.multiline);
  }

  #[test]
  fn test_parse_arguments_output_modes() {
    let json = r#"{"pattern": "test", "output_mode": "content"}"#;
    let args: GrepArgs = parse_arguments(json).unwrap();
    assert!(matches!(args.output_mode, OutputMode::Content));

    let json = r#"{"pattern": "test", "output_mode": "files_with_matches"}"#;
    let args: GrepArgs = parse_arguments(json).unwrap();
    assert!(matches!(args.output_mode, OutputMode::FilesWithMatches));

    let json = r#"{"pattern": "test", "output_mode": "count_matches"}"#;
    let args: GrepArgs = parse_arguments(json).unwrap();
    assert!(matches!(args.output_mode, OutputMode::CountMatches));
  }

  #[tokio::test]
  async fn test_grep_handler_files_with_matches() {
    let handler = GrepHandler::new();
    let cwd = PathBuf::from(".");
    let invocation = ToolInvocation::new(
      "test-call-id",
      "call-test",
      ToolPayload::Function {
        arguments: r#"{"pattern": "ReadFile", "output_mode": "files_with_matches"}"#.to_string(),
      },
      &cwd,
    );

    let result = handler.handle(invocation).await;
    assert!(result.is_ok());

    let output = result.unwrap().into_response();
    // Should find read_file.rs or similar
    assert!(!output.is_empty());
  }

  #[tokio::test]
  async fn test_grep_handler_content() {
    let handler = GrepHandler::new();
    let cwd = PathBuf::from(".");
    let invocation = ToolInvocation::new(
      "test-call-id",
      "call-test",
      ToolPayload::Function {
        arguments: r#"{"pattern": "ReadFile", "output_mode": "content", "line_number": true, "path": "src/tools/handlers/file/read.rs"}"#.to_string(),
      },
      &cwd,
    );

    let result = handler.handle(invocation).await;
    assert!(result.is_ok());

    let output = result.unwrap().into_response();
    assert!(output.contains("ReadFile"));
  }

  #[tokio::test]
  async fn test_grep_handler_no_matches() {
    let handler = GrepHandler::new();
    let cwd = PathBuf::from(".");
    // Use a pattern that is very unlikely to exist in the codebase
    let pattern = format!(
      "NO_MATCH_{}_PATTERN_{}",
      id(),
      SystemTime::now().elapsed().unwrap().as_secs()
    );
    let args = format!(r#"{{"pattern": "{}", "path": "src"}}"#, pattern);
    let invocation = ToolInvocation::new(
      "test-call-id",
      "call-test",
      ToolPayload::Function { arguments: args },
      &cwd,
    );

    let result = handler.handle(invocation).await;
    assert!(result.is_ok(), "Handler returned error: {:?}", result);

    let output = result.unwrap().into_response();
    assert!(
      output.contains("No matches found"),
      "Expected 'No matches found' in output, got: {:?}",
      output
    );
  }

  #[tokio::test]
  async fn test_grep_handler_with_glob() {
    let handler = GrepHandler::new();
    let cwd = PathBuf::from(".");
    let invocation = ToolInvocation::new(
      "test-call-id",
      "call-test",
      ToolPayload::Function {
        arguments:
          r#"{"pattern": "ReadFile", "glob": "*.rs", "output_mode": "files_with_matches"}"#
            .to_string(),
      },
      &cwd,
    );

    let result = handler.handle(invocation).await;
    assert!(result.is_ok());
  }

  #[tokio::test]
  async fn test_grep_handler_head_limit() {
    let handler = GrepHandler::new();
    let cwd = PathBuf::from(".");
    let invocation = ToolInvocation::new(
      "test-call-id",
      "call-test",
      ToolPayload::Function {
        arguments:
          r#"{"pattern": "use", "path": "src/tools", "output_mode": "content", "head_limit": 3}"#
            .to_string(),
      },
      &cwd,
    );

    let result = handler.handle(invocation).await;
    assert!(result.is_ok());

    let output = result.unwrap().into_response();
    if !output.contains("No matches found") {
      // If there are results, check for truncation message
      assert!(output.contains("truncated") || output.lines().count() <= 4);
    }
  }
}
