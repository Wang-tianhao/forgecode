use std::sync::Arc;

use colored::Colorize;
use forge_api::API;
use forge_config::{Update, UpdateFrequency};
use forge_select::ForgeWidget;
use forge_tracker::VERSION;
use update_informer::{Check, Version, registry};

/// Runs the local forge-update.sh script which:
///   1. Fetches upstream main and tags.
///   2. Rebases (or merges) local commits on top of upstream/main.
///   3. Builds the binary with the upstream release version.
///   4. Installs the binary locally.
///   5. Pushes the updated branch to origin.
///   6. Creates/uploads a GitHub release on the fork.
///
/// When `auto_update` is true, exits immediately after a successful update
/// without prompting the user.
async fn execute_update_command(api: Arc<impl API>, auto_update: bool) {
    let command = r#"
        REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
        if [ -z "$REPO_ROOT" ] || [ ! -f "$REPO_ROOT/scripts/forge-update.sh" ]; then
            echo "error: forge update must be run from inside the forge repository clone" >&2
            exit 1
        fi
        bash "$REPO_ROOT/scripts/forge-update.sh"
    "#;

    let output = api.execute_shell_command_raw(command).await;

    match output {
        Err(err) => {
            let _ = send_update_failure_event(&format!("Update failed: {err}")).await;
        }
        Ok(output) => {
            if output.success() {
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
            } else {
                let exit_output = match output.code() {
                    Some(code) => format!("Process exited with code: {code}"),
                    None => "Process exited without code".to_string(),
                };
                let _ = send_update_failure_event(&format!("Update failed, {exit_output}")).await;
            }
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

    let informer = update_informer::new(registry::GitHub, "tailcallhq/forgecode", VERSION)
        .interval(frequency.into());

    if let Some(version) = informer.check_version().ok().flatten()
        && (auto_update || confirm_update(version).await)
    {
        execute_update_command(api, auto_update).await;
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
}
