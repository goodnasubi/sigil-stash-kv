use std::env;
use std::path::Path;
use std::process::{Command, Output};

fn sigil(args: &[&str]) -> Output {
    let executable = env::var_os("SIGIL").unwrap_or_else(|| "sigil".into());
    let output = Command::new(&executable)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("could not run {executable:?}: {error}"));
    assert!(
        output.status.success(),
        "sigil {args:?} failed with {}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

#[test]
fn memory_contract() {
    let entry = "tests/contract.sigil";
    sigil(&["fmt", "--check", "stash/memory.sigil", entry]);
    sigil(&["check", "--root", ".", entry]);

    let output = sigil(&["run", "--root", ".", entry, "main"]);
    let result = String::from_utf8_lossy(&output.stdout);
    assert_eq!(result.trim(), "0", "Sigil evaluator contract result");

    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
    std::fs::create_dir_all(&out).expect("create native test output directory");
    let binary = out.join(format!("contract-native-{}", std::process::id()));
    sigil(&[
        "build",
        "--root",
        ".",
        entry,
        "main",
        "-o",
        binary.to_str().expect("UTF-8 test binary path"),
    ]);
    let status = Command::new(&binary).status();
    std::fs::remove_file(binary).expect("remove native test binary");
    assert_eq!(status.expect("run native contract tests").code(), Some(0));
}
