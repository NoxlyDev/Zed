#!/usr/bin/env bash
# Usage: ./apply.sh /path/to/zed-repo-root
set -euo pipefail
repo="${1:?usage: apply.sh /path/to/zed-repo-root}"
here="$(cd "$(dirname "$0")" && pwd)"
cd "$repo"
patch -p1 --forward < "$here/modified-files.patch"
cp -r "$here/overlay/." "$repo/"
echo "Done. Commit and push, then run the 'Android APK' workflow."
