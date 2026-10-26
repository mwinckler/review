use std::{fs, path::Path, process::Command};

use tempfile::TempDir;

pub fn jj(root: &Path, args: &[&str]) -> String {
    let output = Command::new("jj")
        .current_dir(root)
        .env("JJ_CONFIG", "/dev/null")
        .args([
            "--no-pager",
            "--config",
            "user.name=Review Tests",
            "--config",
            "user.email=review@example.com",
        ])
        .args(args)
        .output()
        .expect("jj must be installed for backend integration tests");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn fixture() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    jj(dir.path(), &["git", "init", "--colocate", "."]);
    fs::create_dir(dir.path().join("src")).unwrap();
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() { println!(\"old\"); }\n",
    )
    .unwrap();
    fs::write(dir.path().join("removed.txt"), "remove me\n").unwrap();
    fs::write(dir.path().join("rename.txt"), "move me\n").unwrap();
    fs::write(dir.path().join("image.bin"), [0, 1, 2, 3]).unwrap();
    jj(dir.path(), &["commit", "-m", "base"]);
    jj(dir.path(), &["bookmark", "create", "main", "-r", "@-"]);
    fs::write(
        dir.path().join("src/main.rs"),
        "fn main() { println!(\"new\"); }\n",
    )
    .unwrap();
    fs::remove_file(dir.path().join("removed.txt")).unwrap();
    fs::rename(
        dir.path().join("rename.txt"),
        dir.path().join("renamed.txt"),
    )
    .unwrap();
    fs::write(dir.path().join("space and\nλ.txt"), "new file\n").unwrap();
    fs::write(dir.path().join("image.bin"), [0, 4, 5, 6]).unwrap();
    jj(dir.path(), &["commit", "-m", "feature"]);
    jj(dir.path(), &["bookmark", "create", "feature", "-r", "@-"]);
    dir
}
