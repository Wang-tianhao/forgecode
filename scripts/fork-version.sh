#!/usr/bin/env bash
# Print the complete release tag for an upstream version. Already-qualified
# tags are preserved so downloads and CI can select an exact historical release.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1#v}"
LABEL="${FORK_LABEL-wang}"

if [[ -n "$LABEL" && "$VERSION" != *"-$LABEL."* ]]; then
    FORK_VERSION="${FORK_VERSION-$(cat "$REPO_ROOT/FORK_VERSION")}"
    if [[ ! "$FORK_VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
        echo "error: FORK_VERSION must be MAJOR.MINOR.PATCH without leading zeros" >&2
        exit 1
    fi
    # Upgrade the old label-only format rather than appending it twice.
    VERSION="${VERSION%-$LABEL}-$LABEL.$FORK_VERSION"
fi

printf 'v%s\n' "$VERSION"
