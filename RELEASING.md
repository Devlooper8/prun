# Releasing Prun

Prun ships as platform installers built by CI. This document covers cutting a
release, code signing, and turning on signed auto-updates. None of the signing
pieces are required to ship — without secrets you still get working *unsigned*
installers; users just see the OS "unknown publisher" warnings.

## Cut a release

1. Bump the version in **all three** manifests (keep them in sync):
   - `src-tauri/Cargo.toml` → `version`
   - `package.json` → `version`
   - `src-tauri/tauri.conf.json` → `version`
2. Move the `## [Unreleased]` notes in `CHANGELOG.md` under a new
   `## [x.y.z] - YYYY-MM-DD` heading.
3. Commit, then tag and push:
   ```bash
   git tag v0.2.0
   git push origin v0.2.0
   ```
4. The **Release** workflow (`.github/workflows/release.yml`) builds installers on
   macOS, Linux, and Windows and opens a **draft** GitHub Release with the
   artifacts attached. Review it, attach the SBOM (below), and publish.

`workflow_dispatch` lets you run the same job manually for a dry run.

## Provenance & SBOM (automatic, no secrets)

Every release build also produces supply-chain evidence:

- **Build provenance attestations** — each installer (msi / setup.exe / deb /
  rpm / AppImage / dmg) is attested via GitHub's sigstore integration. Anyone
  can verify a downloaded artifact really came from this repo's release
  workflow:
  ```bash
  gh attestation verify prun_0.2.0_x64.msi --repo <owner>/prun
  ```
- **CycloneDX SBOM** — the ubuntu leg uploads
  `prun-vX.Y.Z-sbom.cdx.json` as a *workflow artifact* (lockfile-driven, so one
  platform is representative). When publishing the draft release, download it
  from the workflow run and attach it to the release so it ships alongside the
  installers.

CI/release actions are pinned to commit SHAs; Dependabot's `github-actions`
ecosystem keeps the pins current. When reviewing those PRs, check the diff is
only a pin bump.

## Code signing

Unsigned installers work but trip SmartScreen (Windows) and Gatekeeper (macOS).
Add the secrets below in **Settings → Secrets and variables → Actions**; the
release workflow already wires them as env vars and skips signing for any that
are unset.

### Windows (Authenticode)

Tauri signs the installer when the bundle is configured with your certificate.
The common CI route uses an Azure Trusted Signing or a PFX certificate. With a
base64-encoded PFX:

1. Add secrets `WINDOWS_CERTIFICATE` (base64 of the `.pfx`) and
   `WINDOWS_CERTIFICATE_PASSWORD`.
2. In `src-tauri/tauri.conf.json`, set `bundle.windows.certificateThumbprint` (or
   wire a `signCommand`) — see the Tauri Windows code-signing guide. Pass the
   decoded cert into the signing tool in a CI step before `tauri-action`.

> EV/OV certificates remove the SmartScreen reputation prompt fastest. Azure
> Trusted Signing is the lowest-friction modern option.

### macOS (Developer ID + notarization)

Set these repo secrets (the workflow already reads them):

| Secret | What it is |
| --- | --- |
| `APPLE_CERTIFICATE` | base64 of your Developer ID Application `.p12` |
| `APPLE_CERTIFICATE_PASSWORD` | the `.p12` password |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Name (TEAMID)` |
| `APPLE_ID` | your Apple ID email (for notarization) |
| `APPLE_PASSWORD` | an app-specific password for that Apple ID |
| `APPLE_TEAM_ID` | your 10-character Apple Team ID |

With these present, `tauri-action` signs and notarizes the `.app`/`.dmg`
automatically.

## Auto-updates (optional)

The updater plugin is already wired in `src-tauri/src/lib.rs`; it does nothing
until you configure an endpoint and a signing key. To turn it on:

1. **Generate an updater keypair** (local crypto, keep the private key safe):
   ```bash
   npm run tauri signer generate -- -w ~/.tauri/prun.key
   ```
   This prints a **public** key and writes the **private** key to the file.

2. **Add updater config** to `src-tauri/tauri.conf.json`:
   ```jsonc
   {
     "bundle": { "createUpdaterArtifacts": true },
     "plugins": {
       "updater": {
         "pubkey": "<the PUBLIC key from step 1>",
         "endpoints": [
           "https://your-host/prun/{{target}}/{{arch}}/{{current_version}}"
         ]
       }
     }
   }
   ```
   The public key is safe to commit. Host a `latest.json` (Tauri's update
   manifest) at the endpoint; the release workflow uploads the signed update
   artifacts to the GitHub Release, which you can point the endpoint at.

3. **Provide the private key to CI** as secrets (the workflow already reads them):
   - `TAURI_SIGNING_PRIVATE_KEY` — contents of `~/.tauri/prun.key`
   - `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` — its password (empty if none)

4. **Allow the frontend to call the updater** by adding `updater:default` to
   `src-tauri/capabilities/default.json` `permissions`.

5. **Check for updates** from the UI, e.g.:
   ```ts
   const { check } = await import("@tauri-apps/plugin-updater");
   const update = await check();
   if (update) { await update.downloadAndInstall(); }
   ```

> Never commit the private updater key or the Apple `.p12`. Only the updater
> **public** key belongs in version control.

## Secret reference

| Secret | Used for | Required? |
| --- | --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` (+ `_PASSWORD`) | signing auto-updates | only with the updater |
| `APPLE_CERTIFICATE` (+ `_PASSWORD`), `APPLE_SIGNING_IDENTITY` | macOS app signing | macOS signed builds |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | macOS notarization | macOS signed builds |
| `WINDOWS_CERTIFICATE` (+ `_PASSWORD`) | Windows Authenticode | Windows signed builds |
