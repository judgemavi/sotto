# macOS signing and notarization

Sotto's release workflow produces `Sotto-macos-arm64.zip`: a ZIP containing a
Developer ID-signed, hardened-runtime `Sotto.app` with a stapled Apple notarization
ticket. Its sibling `.sha256` file is the integrity input reserved for the Phase 4
signed update manifest. The updater manifest itself is intentionally not implemented
here.

Signing is part of capture correctness, not only distribution. macOS privacy grants
are tied to an app's identity and code signature, so the capture soak test should use
the same identity consistently across rebuilds whenever possible.

## GitHub Actions secrets

Configure these repository or release-environment secrets:

| Secret | Purpose |
| --- | --- |
| `MACOS_CERTIFICATE_BASE64` | Base64 encoding of a Developer ID Application `.p12` export, including its private key. |
| `MACOS_CERTIFICATE_PASSWORD` | Password chosen while exporting that `.p12`. |
| `MACOS_SIGNING_IDENTITY` | Full identity, such as `Developer ID Application: Example Corp (TEAMID)`. |
| `MACOS_KEYCHAIN_PASSWORD` | A random, CI-only password used for the temporary keychain. |
| `APPLE_API_PRIVATE_KEY_BASE64` | Base64 encoding of the App Store Connect API `.p8` private key. |
| `APPLE_API_KEY_ID` | Key ID shown for the App Store Connect API key. |
| `APPLE_API_ISSUER_ID` | Issuer ID from App Store Connect Users and Access → Integrations. |

Create the Developer ID Application certificate in the Apple Developer certificate
portal, install it and its private key in Keychain Access, then export both as a
password-protected `.p12`. Encode it without line wrapping:

```sh
base64 < DeveloperIDApplication.p12 | tr -d '\n'
```

Create an App Store Connect team API key with permission to submit builds. Download
the `.p8` immediately—Apple permits only one download—and encode it the same way. Keep
the original keys in the team's secrets manager; GitHub is a deployment copy, not the
source of record.

The signing script imports the `.p12` into a newly created temporary keychain, adds it
to the keychain search list, signs and verifies the requested path, then restores the
original search list and deletes the keychain even when signing fails. It never writes
the certificate into the repository.

## Local capture-spike signing

T002 can apply the production entitlements to its soak executable without importing a
certificate:

```sh
MACOS_SIGNING_IDENTITY=- scripts/sign.sh target/release/examples/soak
```

That is ad-hoc signing: it is enough for iterative local permission testing but cannot
be notarized or distributed. For stable TCC identity across rebuilds, set the full
Developer ID identity and the three `MACOS_CERTIFICATE_*`/keychain variables expected
by `sign.sh`, matching CI. The single positional argument may be a Mach-O executable
or an `.app` bundle. A missing target, missing entitlement file, certificate import
failure, signing failure, or strict verification failure returns non-zero.

There is no ScreenCaptureKit entitlement for Developer ID distribution. Screen and
system-audio access is governed by Transparency, Consent, and Control (TCC) and the
honest usage description in `Info.plist`. The audio-input entitlement enables the
microphone. Sotto must still request both permissions at runtime and visibly reflect
their state.

## Manual release gate

Pushing a `v*` tag runs `.github/workflows/release.yml`. It builds an arm64 app,
assembles the bundle, signs it, submits a ZIP to `notarytool`, staples the accepted
ticket to the app, packages the final ZIP, and attaches the ZIP plus SHA-256 to the
GitHub release. Do not tag a release until CI secrets are configured and the capture
permission copy has been reviewed.

`notarize.sh` accepts the submission archive and an optional separate staple target:

```sh
scripts/notarize.sh dist/Sotto-notarization.zip dist/Sotto.app
```

It uses an App Store Connect API key, waits synchronously, prints the JSON result, and
fetches the full notarization log when the submission is rejected or the command
fails. Rejection is always fatal. A successful run also validates the stapled ticket.

Before announcing a release, download the published ZIP on a clean Mac, extract it,
and run:

```sh
codesign --verify --deep --strict --verbose=2 Sotto.app
spctl --assess --type execute --verbose=4 Sotto.app
xcrun stapler validate Sotto.app
```

The acceptance gate is `spctl` reporting `accepted` with a notarized Developer ID
origin. CI exercises the same checks, but the clean-machine check catches quarantine
and packaging mistakes.

## Rotation

For a certificate rotation, create and export the replacement first, update the four
`MACOS_*` secrets together, run a tagged release candidate, and only then revoke the
old certificate. Existing signed releases remain verifiable after normal certificate
expiry, but revocation has broader consequences and should be coordinated.

For an App Store Connect key rotation, create the replacement key, update the key,
key-ID, and issuer secrets as one change, verify a notarization, then revoke the old
key. API private keys cannot be recovered; losing the `.p8` requires rotation.

## Troubleshooting CI-only failures

- **`The specified item could not be found in the keychain`:** confirm the `.p12`
  contains the private key, the export password is exact, and the identity string
  matches `security find-identity -v -p codesigning` output.
- **`User interaction is not allowed`:** the temporary keychain was not unlocked or
  its partition list was not set. Inspect the `security import` and
  `set-key-partition-list` steps; never work around this by weakening codesigning.
- **Notary authentication errors:** verify all three API values belong to the same
  App Store Connect organization and that the key has not been revoked.
- **Notarization rejection:** inspect the full log emitted by `notarize.sh`. Common
  causes are unsigned nested code, a missing hardened-runtime option, or an invalid
  bundle identifier. Fix the bundle; do not skip notarization.
- **Local signing works but CI does not:** re-encode secrets without newlines, check
  that GitHub environment protections expose them on tag workflows, and compare the
  pinned Xcode version (`26.6`) with the local toolchain. GPUI also requires the
  separately downloaded Metal Toolchain; CI installs it explicitly with
  `xcodebuild -downloadComponent MetalToolchain`.
- **`spctl` rejects the downloaded app while CI passed:** ensure the final ZIP was
  created *after* stapling and test the GitHub-hosted artifact rather than the local
  pre-notarization bundle.
