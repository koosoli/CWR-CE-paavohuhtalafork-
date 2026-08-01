use std::path::Path;
use std::process::Command;

fn git_revision(repo: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(repo)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        None
    }
}

fn main() {
    let manifest = std::path::PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("manifest directory"),
    );
    let repo = manifest.ancestors().nth(3).expect("repository root");
    let revision = git_revision(repo).unwrap_or_else(|| "unknown".to_owned());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_owned());
    println!("cargo:rustc-env=WGR_BUILD_ID={revision}-{profile}");
    // A checked-out ref moves by updating HEAD or its referenced file. The source
    // build still records `unknown` safely when git metadata is unavailable.
    println!("cargo:rerun-if-changed={}", repo.join(".git/HEAD").display());
    println!("cargo:rerun-if-env-changed=PROFILE");
    // Test-only override used to produce a compatible-import ABI mismatch DLL.
    // It is never set by CMake or a normal Cargo invocation.
    println!("cargo:rerun-if-env-changed=WGR_TEST_ABI_VERSION");
}
