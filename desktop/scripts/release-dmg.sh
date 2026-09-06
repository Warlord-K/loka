#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
repo=${LOKA_RELEASE_REPO:-Warlord-K/loka-releases}
version=${1:-}
cd "$root"

git fetch --quiet origin main
sha=$(git rev-parse origin/main)
day=$(date +%F)
tag=nightly-$day

if [ -n "$version" ]; then
  case "$version" in
    v[0-9]*) ;;
    *) echo "usage: $0 [vX.Y.Z]" >&2; exit 2 ;;
  esac
  tag=$version
fi

last=$(gh release list --repo "$repo" --limit 100 --json tagName,publishedAt --jq '[.[] | select(.tagName | startswith("nightly-"))] | sort_by(.publishedAt) | reverse | .[0].tagName // empty')
last_sha=
if [ -n "$last" ]; then
  last_sha=$(gh release view "$last" --repo "$repo" --json body --jq '.body' | sed -n 's/^commit: \([0-9a-f]\{40\}\)$/\1/p' | head -n 1)
  if [ -z "$version" ] && [ "$last_sha" = "$sha" ]; then
    echo SKIP
    exit 0
  fi
fi

if [ -z "${APPLE_SIGNING_IDENTITY:-}" ] && [ -z "${APPLE_CERTIFICATE:-}" ]; then
  ident=$(security find-identity -p codesigning -v 2>/dev/null | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -n 1)
  if [ -n "$ident" ]; then
    export APPLE_SIGNING_IDENTITY="$ident"
  fi
fi

signed=0
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ "$APPLE_SIGNING_IDENTITY" != "-" ]; then
  signed=1
elif [ -n "${APPLE_CERTIFICATE:-}" ]; then
  signed=1
fi
notarized=0
if [ "$signed" -eq 1 ]; then
  if [ -n "${APPLE_KEYCHAIN_PROFILE:-}" ]; then
    notarized=1
  elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
    notarized=1
  elif [ -n "${APPLE_API_KEY:-}" ] && [ -n "${APPLE_API_ISSUER:-}" ]; then
    notarized=1
  fi
fi

if [ "$signed" -eq 0 ] && [ -z "${LOKA_ALLOW_UNSIGNED:-}" ]; then
  echo "no Developer ID Application certificate found; set LOKA_ALLOW_UNSIGNED=1 to publish an unsigned build" >&2
  exit 1
fi

if [ -n "$last_sha" ] && git cat-file -e "${last_sha}^{commit}" 2>/dev/null; then
  changelog=$(git log --format='%s' "${last_sha}..${sha}" | grep -E '\(#[0-9]+\)$' || true)
  changelog=$(printf '%s\n' "$changelog" | sed '/^$/d' | awk 'NR>1 {printf "; "} {printf "%s", $0} END {if (NR) printf "\n"}')
  if [ -z "$changelog" ]; then
    changelog="No pull requests merged."
  fi
else
  changelog="First build."
fi

work=$(mktemp -d "${TMPDIR:-/tmp}/loka-nightly.XXXXXX")
cleanup() {
  git worktree remove --force "$work" >/dev/null 2>&1 || rm -rf "$work"
  git worktree prune >/dev/null 2>&1 || true
}
trap cleanup EXIT
git worktree add --detach "$work" "$sha" >&2

export CI="${CI:-true}"
if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && [ -f "$HOME/.tauri/loka-updater.key" ]; then
  export TAURI_SIGNING_PRIVATE_KEY="$HOME/.tauri/loka-updater.key"
  export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}"
fi
build_config="{}"
if [ -n "$version" ]; then
  if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
    echo "versioned releases need the update signing key (TAURI_SIGNING_PRIVATE_KEY or ~/.tauri/loka-updater.key)" >&2
    exit 1
  fi
  build_config="{\"version\":\"${version#v}\",\"bundle\":{\"createUpdaterArtifacts\":true}}"
fi
(
  cd "$work/desktop"
  npm ci
  npm run build -- --config "$build_config"
) >&2

dmg_dir="$work/desktop/src-tauri/target/release/bundle/dmg"
if [ ! -d "$dmg_dir" ] || ! ls "$dmg_dir"/*.dmg >/dev/null 2>&1; then
  echo "no DMG was produced" >&2
  exit 1
fi
dmg=$(ls -t "$dmg_dir"/*.dmg | head -n 1)
arch=$(uname -m)
if [ -n "$version" ]; then
  asset_name="Loka-${version}-${arch}.dmg"
  title="Loka ${version}"
else
  asset_name="Loka-nightly-${day}-${arch}.dmg"
  title="Loka nightly ${day}"
fi
asset="$work/$asset_name"
cp "$dmg" "$asset"

if [ "$signed" -eq 1 ] && [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ "$APPLE_SIGNING_IDENTITY" != "-" ]; then
  codesign --force --timestamp --sign "$APPLE_SIGNING_IDENTITY" "$asset" >&2
fi
if [ "$notarized" -eq 1 ]; then
  if [ -n "${APPLE_KEYCHAIN_PROFILE:-}" ]; then
    xcrun notarytool submit "$asset" --keychain-profile "$APPLE_KEYCHAIN_PROFILE" --wait >&2
  elif [ -n "${APPLE_API_KEY:-}" ]; then
    xcrun notarytool submit "$asset" --key "${APPLE_API_KEY_PATH:-$HOME/.appstoreconnect/private_keys/AuthKey_${APPLE_API_KEY}.p8}" --key-id "$APPLE_API_KEY" --issuer "$APPLE_API_ISSUER" --wait >&2
  else
    xcrun notarytool submit "$asset" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" --wait >&2
  fi
  xcrun stapler staple "$asset" >&2
  spctl --assess --type open --context context:primary-signature --verbose "$asset" >&2
fi

notes="$work/notes.txt"
{
  printf 'commit: %s\n\n%s\n' "$sha" "$changelog"
  if [ "$notarized" -eq 0 ]; then
    printf '\nNot notarized. Open the DMG, drag Loka to Applications, then right-click Loka and choose Open.\n'
  fi
} > "$notes"

if gh release view "$tag" --repo "$repo" >/dev/null 2>&1; then
  gh release delete "$tag" --repo "$repo" --yes --cleanup-tag >&2
fi
target=
if [ "$(gh repo view --json nameWithOwner --jq .nameWithOwner 2>/dev/null)" = "$repo" ]; then
  target="--target $sha"
fi
if [ -n "$version" ]; then
  stable="$work/Loka.dmg"
  cp "$asset" "$stable"
  bundle_dir="$work/desktop/src-tauri/target/release/bundle/macos"
  archive=$(ls -t "$bundle_dir"/*.app.tar.gz | head -n 1)
  update_name="Loka-${version}-${arch}.app.tar.gz"
  update_asset="$work/$update_name"
  cp "$archive" "$update_asset"
  case "$arch" in
    arm64) platform=darwin-aarch64 ;;
    *) platform=darwin-x86_64 ;;
  esac
  headline=$(sed -n 's/^    title: "\(.*\)",$/\1/p' "$work/frontend/lib/changelog.ts" | head -n 1)
  node -e '
    const fs = require("fs");
    const [version, notes, sigFile, platform, url, out] = process.argv.slice(1);
    const manifest = { version, notes, pub_date: new Date().toISOString(), platforms: { [platform]: { signature: fs.readFileSync(sigFile, "utf8").trim(), url } } };
    fs.writeFileSync(out, JSON.stringify(manifest, null, 2));
  ' "${version#v}" "${headline:-$changelog}" "$archive.sig" "$platform" "https://github.com/${repo}/releases/download/${tag}/${update_name}" "$work/latest.json"
  gh release create "$tag" --repo "$repo" $target --title "$title" --notes-file "$notes" "$asset" "$stable" "$update_asset" "$work/latest.json" >&2
else
  gh release create "$tag" --repo "$repo" --prerelease $target --title "$title" --notes-file "$notes" "$asset" >&2
fi
rm -f "$notes"

old=$(gh release list --repo "$repo" --limit 100 --json tagName,publishedAt --jq '[.[] | select(.tagName | startswith("nightly-"))] | sort_by(.publishedAt) | reverse | .[7:][].tagName')
for stale in $old; do
  gh release delete "$stale" --repo "$repo" --yes --cleanup-tag >&2 || printf 'could not delete %s\n' "$stale" >&2
done

url="https://github.com/${repo}/releases/download/${tag}/${asset_name}"
printf '%s\n%s\ncommit %s\n%s\n' "$title" "$url" "$sha" "$changelog"
if [ "$notarized" -eq 0 ]; then
  printf 'Not notarized. Right-click Loka and choose Open the first time.\n'
fi
