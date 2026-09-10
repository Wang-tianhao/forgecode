use std::sync::Arc;

use colored::Colorize;
use forge_api::API;
use forge_config::{Update, UpdateFrequency};
use forge_select::ForgeWidget;
use forge_tracker::VERSION;
use update_informer::{Check, Version, registry};

/// GitHub repository that hosts this fork's releases. This is the
/// distribution channel for custom builds: `forge update` checks it for new
/// versions and downloads pre-built binaries from it. Override at compile
/// time with the `FORK_REPO` environment variable.
const FORK_REPO: &str = match option_env!("FORK_REPO") {
    Some(repo) => repo,
    None => "Wang-tianhao/forgecode",
};

/// Computes the release asset name for the current platform, matching the
/// naming scheme used by `scripts/forge-update.sh` when publishing releases.
fn asset_name() -> String {
    let arch = std::env::consts::ARCH;
    match std::env::consts::OS {
        "macos" => format!("forge-{arch}-apple-darwin"),
        "linux" => format!("forge-{arch}-unknown-linux-gnu"),
        other => format!("forge-{arch}-{other}"),
    }
}

/// Normalizes a version string into a git tag by ensuring it has a 'v'
/// prefix, matching the tags created by `scripts/forge-update.sh`.
fn release_tag(version: &str) -> String {
    if version.starts_with('v') { version.to_string() } else { format!("v{version}") }
}

/// Builds a shell command that downloads the pre-built binary for the current
/// platform from the fork's GitHub release and installs it over the currently
/// running executable, re-signing it on macOS.
///
/// # Errors
///
/// Returns an error if the path of the current executable cannot be
/// determined.
fn download_command(version: &str) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    let exe = exe.display();
    let asset = asset_name();
    let tag = release_tag(version);
    Ok(format!(
        r#"
        set -e
        TMP_BIN="$(mktemp)"
        curl -fsSL "https://github.com/{FORK_REPO}/releases/download/{tag}/{asset}" -o "$TMP_BIN"
        install -m 755 "$TMP_BIN" "{exe}"
        rm -f "$TMP_BIN"
        if command -v codesign >/dev/null 2>&1; then
            codesign -f -s - "{exe}"
        fi
        "#
    ))
}

/// Attempts to run a shell command, returning whether it succeeded. Failures
/// are reported to the tracker and swallowed, since update is best-effort.
async fn run_command(api: &Arc<impl API>, command: &str) -> bool {
    match api.execute_shell_command_raw(command).await {
        Ok(output) if output.success() => true,
        Ok(output) => {
            let exit_output = match output.code() {
                Some(code) => format!("Process exited with code: {code}"),
                None => "Process exited without code".to_string(),
            };
            let _ = send_update_failure_event(&format!("Update failed, {exit_output}")).await;
            false
        }
        Err(err) => {
            let _ = send_update_failure_event(&format!("Update failed: {err}")).await;
            false
        }
    }
}

/// Updates forge by downloading the pre-built binary from the fork's GitHub
/// release for the given version and installing it over the current
/// executable. If the download fails (e.g. the release or platform asset does
/// not exist yet), falls back to running `scripts/forge-update.sh`, which
/// syncs upstream, builds from source, and publishes the release. The
/// fallback requires the current working directory to be inside the forge
/// repository clone.
///
/// When `auto_update` is true, exits immediately after a successful update
/// without prompting the user.
async fn execute_update_command(api: Arc<impl API>, auto_update: bool, version: Version) {
    // Fast path: download the pre-built binary published by the fork.
    let mut success = match download_command(&version.to_string()) {
        Ok(command) => run_command(&api, &command).await,
        Err(err) => {
            let _ = send_update_failure_event(&format!("Update failed: {err}")).await;
            false
        }
    };

    // Fallback: full sync + build via the repo script.
    if !success {
        let command = r#"
            REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
            if [ -z "$REPO_ROOT" ] || [ ! -f "$REPO_ROOT/scripts/forge-update.sh" ]; then
                echo "error: fork release unavailable and forge update must be run from inside the forge repository clone to build from source" >&2
                exit 1
            fi
            bash "$REPO_ROOT/scripts/forge-update.sh"
        "#;
        success = run_command(&api, command).await;
    }

    if success {
        let should_exit = if auto_update {
            true
        } else {
            let answer = ForgeWidget::confirm(
                "Update completed. You need to restart forge to use the new version. Exit now?",
            )
            .with_default(true)
            .prompt();
            answer.unwrap_or_default().unwrap_or_default()
        };
        if should_exit {
            std::process::exit(0);
        }
    }
}

async fn confirm_update(version: Version) -> bool {
    let answer = ForgeWidget::confirm(format!(
        "Confirm upgrade from {} -> {} (latest)?",
        VERSION.to_string().bold().white(),
        version.to_string().bold().white()
    ))
    .with_default(true)
    .prompt();

    match answer {
        Ok(Some(result)) => result,
        Ok(None) => false, // User canceled
        Err(_) => false,   // Error occurred
    }
}

fn should_check_for_updates(frequency: &UpdateFrequency) -> bool {
    !matches!(frequency, UpdateFrequency::Never)
}

/// Checks if there is an update available
pub async fn on_update(api: Arc<impl API>, update: Option<&Update>) {
    let update = update.cloned().unwrap_or_default();
    let frequency = update.frequency.unwrap_or_default();

    if !should_check_for_updates(&frequency) {
        return;
    }

    let auto_update = update.auto_update.unwrap_or_default();

    // Check if version is development version, in which case we skip the update
    // check
    if VERSION.contains("dev") || VERSION == "0.1.0" {
        // Skip update for development version 0.1.0
        return;
    }

    // Check the fork's releases: a prompt only appears once a downloadable
    // custom build has been published for a new upstream or fork version.
    let informer = update_informer::new(registry::GitHub, FORK_REPO, VERSION)
        .interval(frequency.into());

    if let Some(version) = informer.check_version().ok().flatten()
        && (auto_update || confirm_update(version.clone()).await)
    {
        execute_update_command(api, auto_update, version).await;
    }
}

/// Sends an event to the tracker when an update fails
async fn send_update_failure_event(error_msg: &str) -> anyhow::Result<()> {
    tracing::error!(error = error_msg, "Update failed");
    // Always return Ok since we want to fail silently
    Ok(())
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn test_should_skip_update_check_when_frequency_is_never() {
        let fixture = UpdateFrequency::Never;

        let actual = should_check_for_updates(&fixture);

        let expected = false;
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_release_tag_adds_v_prefix() {
        let fixture = "2.13.21";

        let actual = release_tag(fixture);

        let expected = "v2.13.21";
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_release_tag_keeps_existing_v_prefix() {
        let fixture = "v2.13.21";

        let actual = release_tag(fixture);

        let expected = "v2.13.21";
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_asset_name_matches_platform_naming_scheme() {
        let actual = asset_name();

        let arch = std::env::consts::ARCH;
        let expected = match std::env::consts::OS {
            "macos" => format!("forge-{arch}-apple-darwin"),
            "linux" => format!("forge-{arch}-unknown-linux-gnu"),
            other => format!("forge-{arch}-{other}"),
        };
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_download_command_targets_fork_release_and_current_exe() {
        let fixture = "2.13.21-wang.1.2.3";

        let actual = download_command(fixture).unwrap();

        let exe = std::env::current_exe().unwrap();
        assert!(actual.contains(&format!(
            "github.com/{FORK_REPO}/releases/download/v2.13.21-wang.1.2.3/"
        )));
        assert!(actual.contains(&asset_name()));
        assert!(actual.contains(&exe.display().to_string()));
        assert!(actual.contains("codesign"));
    }
}
