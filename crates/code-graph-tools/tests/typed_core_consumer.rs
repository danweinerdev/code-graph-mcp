//! Cross-crate boundary pin for the public typed core.

use std::path::Path;
use std::process::Command;

#[test]
fn typed_core_consumer_compiles_without_rmcp_dependency() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture = manifest_dir.join("tests/typed_core_consumer/Cargo.toml");
    let manifest = std::fs::read_to_string(&fixture).expect("read typed-core consumer manifest");
    assert!(
        !manifest
            .lines()
            .any(|line| line.trim_start().starts_with("rmcp")),
        "the typed-core consumer must not declare rmcp"
    );

    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string()))
        .args(["check", "--quiet", "--manifest-path"])
        .arg(&fixture)
        .output()
        .expect("run typed-core consumer cargo check");
    assert!(
        output.status.success(),
        "typed-core consumer must compile without rmcp:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
