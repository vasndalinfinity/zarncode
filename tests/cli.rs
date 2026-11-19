use std::process::Command;

fn binary() -> &'static str {
  env!("CARGO_BIN_EXE_zarncode")
}

#[test]
fn help_uses_zarncode_identity() {
  let output = Command::new(binary()).arg("--help").output().unwrap();
  assert!(output.status.success());
  let help = String::from_utf8(output.stdout).unwrap();
  assert!(help.contains("Usage: zarncode"), "{help}");
  assert!(help.contains(".zarncode"), "{help}");
  assert!(!help.contains("ironcode"), "{help}");
}

#[test]
fn version_uses_zarncode_identity() {
  let output = Command::new(binary()).arg("--version").output().unwrap();
  assert!(output.status.success());
  assert!(
    String::from_utf8(output.stdout)
      .unwrap()
      .starts_with("zarncode ")
  );
}

#[test]
fn preview_runs_without_configuration_or_credentials() {
  let directory = tempfile::tempdir().unwrap();
  let output = Command::new(binary())
    .args(["--config", directory.path().to_str().unwrap(), "--preview"])
    .env_remove("KIMI_API_KEY")
    .output()
    .unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let screen = String::from_utf8(output.stdout).unwrap();
  assert!(screen.contains("Zarncode"), "{screen}");
  assert!(screen.contains("OFFLINE PREVIEW"), "{screen}");
  assert!(screen.contains("No provider connected"), "{screen}");
  assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
