#!/bin/sh
set -eu

project_root="$(cd "$(dirname "$0")/../../.." && pwd)"
tauri_dir="$project_root/desktop/src-tauri"
target_triple="$(rustc -vV | sed -n 's/^host: //p')"
api_port=47811
pg_version=17.11.1
node_version=24.22.2
cache="${LOKA_DOWNLOAD_CACHE:-$project_root/target/loka-downloads}"

case "$target_triple" in
  aarch64-apple-darwin)
    pg_sha=f4b2ff0587d80b6f22bf09ec2d133a3c2949b6423318d3fed0f4ddddddf2f004
    node_arch=arm64
    node_sha=aa9549349e47dffaa42dcf95d6e06ade8c4bdafc980979883b88767670df385a
    ;;
  x86_64-apple-darwin)
    pg_sha=edb44b25cf8bffec60fa300fbe100fa4238bf2cab4f4bd98d4b7255c61780df3
    node_arch=x64
    node_sha=d5e336d588c45be0c37dbd52cc29895593009ca4997a77a1fd02815131ed0e52
    ;;
  *)
    echo "Loka desktop builds only for macOS, not $target_triple" >&2
    exit 1
    ;;
esac

fetch() {
  url=$1
  file=$2
  sum=$3
  mkdir -p "$cache"
  if [ -f "$cache/$file" ] && [ "$(shasum -a 256 "$cache/$file" | cut -d ' ' -f 1)" = "$sum" ]; then
    return 0
  fi
  curl -fsSL --retry 3 -o "$cache/$file.part" "$url"
  got=$(shasum -a 256 "$cache/$file.part" | cut -d ' ' -f 1)
  if [ "$got" != "$sum" ]; then
    rm -f "$cache/$file.part"
    echo "checksum mismatch for $file: expected $sum, got $got" >&2
    exit 1
  fi
  mv "$cache/$file.part" "$cache/$file"
}

cargo build --manifest-path "$project_root/Cargo.toml" --package loka-agent --package loka-api --release
mkdir -p "$tauri_dir/binaries"
install -m 0755 "$project_root/target/release/loka-agent" "$tauri_dir/binaries/loka-agent-$target_triple"
install -m 0755 "$project_root/target/release/loka-api" "$tauri_dir/binaries/loka-api-$target_triple"
mkdir -p "$tauri_dir/icons"
if [ ! -f "$tauri_dir/icons/icon-rgba.png" ]; then
  cp "$project_root/frontend/public/logo-192.png" "$tauri_dir/icons/icon-rgba.png"
fi

pg_file="postgresql-$pg_version-$target_triple.tar.gz"
fetch "https://github.com/theseus-rs/postgresql-binaries/releases/download/$pg_version/$pg_file" "$pg_file" "$pg_sha"
node_file="node-v$node_version-darwin-$node_arch.tar.gz"
fetch "https://nodejs.org/dist/v$node_version/$node_file" "$node_file" "$node_sha"

runtime="$tauri_dir/runtime"
work=$(mktemp -d "${TMPDIR:-/tmp}/loka-runtime.XXXXXX")
trap 'rm -rf "$work"' EXIT
rm -rf "$runtime"
mkdir -p "$runtime/postgres/bin" "$runtime/node/bin"

tar -xzf "$cache/$pg_file" -C "$work"
pg_src="$work/postgresql-$pg_version-$target_triple"
for tool in postgres initdb pg_isready; do
  cp "$pg_src/bin/$tool" "$runtime/postgres/bin/$tool"
done
cp "$pg_src/LICENSE" "$runtime/postgres/LICENSE"
cp -R "$pg_src/lib" "$runtime/postgres/lib"
cp -R "$pg_src/share" "$runtime/postgres/share"
rm -rf "$runtime/postgres/lib/pgxs" "$runtime/postgres/lib/bitcode" "$runtime/postgres/lib/python3."* "$runtime/postgres/lib/Python" \
  "$runtime/postgres/lib/libLLVM.dylib" "$runtime/postgres/lib/llvmjit"* "$runtime/postgres/share/doc"
find "$runtime/postgres/lib" -name '*plpython*' -delete
find "$runtime/postgres/lib" -name '*.a' -delete
find "$runtime/postgres/share/extension" -name '*plpython*' -delete

tar -xzf "$cache/$node_file" -C "$work"
node_src="$work/node-v$node_version-darwin-$node_arch"
cp "$node_src/bin/node" "$runtime/node/bin/node"
cp "$node_src/LICENSE" "$runtime/node/LICENSE"

(
  cd "$project_root/frontend"
  npm ci
  API_URL="http://127.0.0.1:$api_port" NEXT_TELEMETRY_DISABLED=1 npm run build
)
standalone="$project_root/frontend/.next/standalone"
if [ ! -f "$standalone/server.js" ]; then
  echo "the frontend build has no .next/standalone/server.js" >&2
  exit 1
fi
cp -R "$standalone" "$runtime/web"
mkdir -p "$runtime/web/.next"
cp -R "$project_root/frontend/.next/static" "$runtime/web/.next/static"
cp -R "$project_root/frontend/public" "$runtime/web/public"
rm -f "$runtime/web/.env" "$runtime/web/.env.local"
if ! "$project_root/desktop/scripts/components.sh" > "$runtime/components.json"; then
  rm -f "$runtime/components.json"
  echo "could not work out component ids, so this build only takes full updates" >&2
fi

if [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
  APPLE_SIGNING_IDENTITY=$(security find-identity -p codesigning -v 2>/dev/null | sed -n 's/.*"\(Developer ID Application:[^"]*\)".*/\1/p' | head -n 1)
fi
if [ -n "${APPLE_SIGNING_IDENTITY:-}" ] && [ "$APPLE_SIGNING_IDENTITY" != "-" ]; then
  cat > "$work/node.entitlements" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>com.apple.security.cs.allow-jit</key>
  <true/>
  <key>com.apple.security.cs.allow-unsigned-executable-memory</key>
  <true/>
</dict>
</plist>
PLIST
  find "$runtime" -type f | while read -r binary; do
    file -b "$binary" | grep -q 'Mach-O' || continue
    if [ "$binary" = "$runtime/node/bin/node" ]; then
      if codesign -dv "$binary" 2>&1 | grep -q 'Authority=Developer ID Application'; then
        continue
      fi
      codesign --force --timestamp --options runtime --entitlements "$work/node.entitlements" --sign "$APPLE_SIGNING_IDENTITY" "$binary"
    else
      codesign --force --timestamp --options runtime --sign "$APPLE_SIGNING_IDENTITY" "$binary"
    fi
  done
fi
