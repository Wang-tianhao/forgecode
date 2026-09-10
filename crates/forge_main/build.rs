fn clean_version(version: &str) -> String {
    // Remove 'v' prefix if present using strip_prefix
    version.strip_prefix('v').unwrap_or(version).to_string()
}

fn main() {
    // Priority order:
    // 1. APP_VERSION environment variable (for CI/CD builds)
    // 2. Fallback to dev version

    let version = std::env::var("APP_VERSION")
        .map(|v| clean_version(&v))
        .unwrap_or_else(|_| "0.1.0-dev".to_string());

    // Append an optional fork label so custom builds are distinguishable from
    // official releases with the same upstream tag (e.g. FORK_LABEL=wang
    // turns 2.13.21 into 2.13.21-wang).
    let version = match std::env::var("FORK_LABEL") {
        Ok(label) if !label.is_empty() => format!("{version}-{}", clean_version(&label)),
        _ => version,
    };

    // Make version available to the application
    println!("cargo:rustc-env=CARGO_PKG_VERSION={version}");

    // Make version available to the application
    println!("cargo:rustc-env=CARGO_PKG_NAME=forge");

    // Ensure rebuild when environment changes
    println!("cargo:rerun-if-env-changed=APP_VERSION");
    println!("cargo:rerun-if-env-changed=FORK_LABEL");
}
