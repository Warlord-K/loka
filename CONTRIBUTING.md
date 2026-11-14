# Contributing to Loka

Thanks for helping. Issues and pull requests are welcome; for anything large, open an issue first so we can agree on the shape.

## License and CLA

Loka is licensed under [AGPL-3.0-only](LICENSE). Contributions require agreeing to a Contributor License Agreement. The CLA is not in the repository yet; it will be added soon, and pull requests are merged only once their author has agreed to it.

## Setup

You need Rust (stable), Node 20 or later, the Xcode command line tools and Postgres 17:

```bash
brew install postgresql@17 && brew services start postgresql@17
createdb loka
```

Run the pieces you are changing:

```bash
DATABASE_URL=postgres://localhost/loka AUTH_DISABLED=true API_BIND_HOST=127.0.0.1 cargo run -p loka-api
cd frontend && npm install && npm run dev
```

The API applies `migrations/` on startup. For agent work you also need a runner. Register one and start it with the returned id and token (one runner per person; revoke the old one first):

```bash
curl -s -X POST http://127.0.0.1:8081/api/world/runners -H 'content-type: application/json' -d '{"name":"dev","platform":"macos"}'
WORLD_RUNNER_URL=http://127.0.0.1:8081 WORLD_RUNNER_ID=... WORLD_RUNNER_TOKEN=... WORLD_RUNNER_ROOT=$HOME/.loka-dev/runner cargo run -p loka-agent -- runner
```

With `AUTH_DISABLED=true` every request is one local user. Keep the API on `127.0.0.1`.

The desktop app builds everything it bundles in its prepare step (`desktop/src-tauri/scripts/prepare-sidecar.sh`, also run by `npm run build`). For a dev window: `cd desktop && npm ci && npm run dev`.

## Before you open a pull request

```bash
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets
cargo test --workspace
cd frontend && npm run build
```

Database tests skip themselves unless their variable points at a migrated database, for example:

```bash
STEER_TEST_DATABASE_URL=postgres://localhost/loka_test cargo test -p loka-shared
```

[CLAUDE.md](CLAUDE.md) lists every test variable, the live CLI checks and the invariants of each subsystem. Read the section for the code you touch.

## House rules

- No code comments. Name things so the code reads on its own.
- No em dashes in text people or agents see; use a hyphen with spaces, a comma or two sentences.
- Never edit a migration that has shipped. Add the next numbered file in `migrations/` (check `ls migrations` for the next free number).
- Commits carry no `Co-Authored-By` trailers.
- Every new claim path skips paused agents, and every new wake goes through the existing `pg_notify` channels instead of a polling loop.
- Anything an agent can create must also be listable, changeable and removable by an agent tool.
- User-visible changes add a line to the newest entry in `frontend/lib/changelog.ts`.
- UI uses the `w-*` colour tokens and Tailwind only, and respects `prefers-reduced-motion`.

## Security

Please do not open public issues for vulnerabilities; follow [SECURITY.md](SECURITY.md).

## Releasing the Mac app

Releases are signed, notarized DMGs published to the public [Warlord-K/loka-releases](https://github.com/Warlord-K/loka-releases/releases) repository, and [loka.ygpt.dev/download](https://loka.ygpt.dev/download) always points at the newest one. The bundle id is `dev.ygpt.loka`; a fork that ships its own app should change it in `desktop/src-tauri/tauri.conf.json`.

1. With a paid Apple Developer account, create a Developer ID Application certificate (Xcode, Settings > Accounts > Manage Certificates, then + and Developer ID Application). Only the account holder can create one. It lands in your login keychain.
2. Save notarization credentials once, using an app-specific password from account.apple.com:

```bash
xcrun notarytool store-credentials loka --apple-id you@example.com --team-id TEAMID
```

3. Keep the update signing key at `~/.tauri/loka-updater.key` (or set `TAURI_SIGNING_PRIVATE_KEY`). Versioned releases sign an update archive with it and publish `latest.json`, which installed apps read to offer the update. Back it up: apps only accept updates signed with this key.
4. Build, sign, notarize, staple and publish:

```bash
APPLE_KEYCHAIN_PROFILE=loka desktop/scripts/release-dmg.sh v0.1.0
```

`prepare-sidecar.sh` signs every bundled binary (Postgres, Node, native web modules) with the hardened runtime before Tauri signs the app, and the script notarizes and staples the DMG. Instead of a keychain profile you can pass `APPLE_ID`, `APPLE_PASSWORD` and `APPLE_TEAM_ID`, or an App Store Connect key with `APPLE_API_KEY` and `APPLE_API_ISSUER` (and `APPLE_API_KEY_PATH` if the `.p8` is not in `~/.appstoreconnect/private_keys`). The script picks the Developer ID identity from the keychain unless `APPLE_SIGNING_IDENTITY` is set, and refuses to publish without one unless `LOKA_ALLOW_UNSIGNED=1`. A versioned release also uploads `Loka.dmg`, the file `/download` serves. Without a version it publishes a `nightly-<date>` prerelease (skipped when `main` has not changed) and keeps the last seven. `LOKA_RELEASE_REPO` overrides the target repository.
