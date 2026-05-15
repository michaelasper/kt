use std::fs;
use std::process::Command;
use tempfile::tempdir;

#[tokio::test]
async fn test_reproduce_issue_48() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("invalid_utf8.rs");

    // Write invalid UTF-8 bytes
    fs::write(&file_path, vec![0, 159, 146, 150]).unwrap();

    let name = format!("repro-48-{}", fastrand::u32(..));

    // Run kt sync on this directory
    let output = Command::new("cargo")
        .args([
            "run",
            "--",
            "sync",
            dir.path().to_str().unwrap(),
            "--name",
            &name,
        ])
        .output()
        .expect("failed to execute process");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    println!("STDOUT: {}", stdout);
    println!("STDERR: {}", stderr);

    // Verify if it reported errors
    assert!(
        stdout.contains("0 chunks indexed, 1 errors") || stdout.contains("0 chunks (1 errors)")
    );
    assert!(stdout.contains("Failed to parse"));
}
