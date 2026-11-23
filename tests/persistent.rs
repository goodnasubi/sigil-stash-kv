use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const ENTRIES: [&str; 2] = [
    "tests/persistent_contract.sigil",
    "tests/persistent_recovery.sigil",
];

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn sigil(root: &Path, args: &[&str]) -> Output {
    let executable = env::var_os("SIGIL").unwrap_or_else(|| "sigil".into());
    let output = Command::new(&executable)
        .current_dir(root)
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

fn pin(name: &str) -> String {
    let text =
        fs::read_to_string(manifest_dir().join("deps/pins.env")).expect("read deps/pins.env");
    let value = text
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .unwrap_or_else(|| panic!("{name} is not set in deps/pins.env"))
        .trim()
        .to_string();
    assert!(
        value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "{name} must be a full 40-digit commit SHA, got {value:?}"
    );
    value
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed in {dir:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The sigil-db checkout: `SIGIL_DB_CORE`, or `.deps/sigil-db-core` from `scripts/fetch-deps.sh`.
/// It must be exactly the pinned commit with no local changes.
fn core_dir() -> PathBuf {
    let dir = env::var_os("SIGIL_DB_CORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir().join(".deps/sigil-db-core"));
    assert!(
        dir.join("db/relation.sigil").is_file(),
        "{dir:?} is not a sigil-db checkout; run scripts/fetch-deps.sh or set SIGIL_DB_CORE"
    );
    let expected = pin("SIGIL_DB_CORE_SHA");
    assert_eq!(
        git(&dir, &["rev-parse", "HEAD"]),
        expected,
        "the sigil-db core is not the pinned commit"
    );
    assert_eq!(
        git(&dir, &["status", "--porcelain"]),
        "",
        "the sigil-db core has local changes"
    );
    dir
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create directory");
    for entry in fs::read_dir(from).expect("read directory") {
        let entry = entry.expect("directory entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// The compiler only reads modules below its root, so the pinned core's `db/` is copied next to
/// this repository's `stash/` and `tests/` in a scratch root.
fn build_root() -> PathBuf {
    let root = manifest_dir().join("target/persistent-root");
    if root.exists() {
        fs::remove_dir_all(&root).expect("clear scratch root");
    }
    copy_tree(&core_dir().join("db"), &root.join("db"));
    copy_tree(&manifest_dir().join("stash"), &root.join("stash"));
    copy_tree(&manifest_dir().join("tests"), &root.join("tests"));
    root
}

#[test]
fn persistent_adapter() {
    let root = build_root();
    sigil(
        &manifest_dir(),
        &[
            "fmt",
            "--check",
            "stash/persistent.sigil",
            "tests/persistent_support.sigil",
            "tests/persistent_contract.sigil",
            "tests/persistent_recovery.sigil",
        ],
    );
    let out = manifest_dir().join("target");
    for entry in ENTRIES {
        sigil(&root, &["check", "--root", ".", entry]);

        let output = sigil(&root, &["run", "--root", ".", entry, "main"]);
        let result = String::from_utf8_lossy(&output.stdout);
        assert_eq!(result.trim(), "0", "{entry}: Sigil evaluator result");

        let binary = out.join(format!("persistent-native-{}", std::process::id()));
        sigil(
            &root,
            &[
                "build",
                "--root",
                ".",
                entry,
                "main",
                "-o",
                binary.to_str().expect("UTF-8 test binary path"),
            ],
        );
        let status = Command::new(&binary).status();
        fs::remove_file(&binary).expect("remove native test binary");
        assert_eq!(
            status.expect("run native test").code(),
            Some(0),
            "{entry}: native exit code"
        );
    }
}
