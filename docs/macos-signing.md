# macOS release signing

The release workflow signs the Apple silicon app and CLI with Developer ID,
notarizes them, and staples the ticket to `Legato.app`. The downloadable package
remains a `.tar.gz`; an intermediate ZIP is submitted to Apple. The bundle ID
stays `io.github.tvolk131.legato`.

## Credentials

Configure these repository Actions secrets:

| Secret | Contents |
| --- | --- |
| `APPLE_CERTIFICATE_P12` | Base64-encoded Developer ID Application certificate and private key (`.p12`) |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12` export password |
| `APPLE_API_PRIVATE_KEY` | The complete App Store Connect team API private key (`.p8`) |
| `APPLE_API_KEY_ID` | The API key's Key ID |
| `APPLE_API_ISSUER_ID` | The API key's Issuer ID |

Set the Actions **variable** `APPLE_TEAM_ID` to the 10-character Apple Developer
Team ID. The API key needs Developer access to notarize software.

Create the certificate using Apple's Developer ID Application flow, then export
the identity with its private key as a password-protected `.p12`. Keep a secure
backup of the `.p12`, its password, and the `.p8` with its identifiers. Key ID,
Issuer ID, and Team ID identify different things.

The runner imports the certificate into a temporary keychain and selects exactly
one valid Developer ID Application identity belonging to `APPLE_TEAM_ID`. It
removes the temporary keychain and credential files when packaging finishes,
including on failure. Signing credentials are used only when packaging tags or
manual builds; main's cache warm-ups do not receive them.

## Validate credentials without publishing

Run the existing **release** workflow manually. Select the branch containing the
workflow to test, enter an existing version tag, and leave **publish** unchecked:

```sh
gh workflow run release.yml --ref main -f tag=v0.3.0-alpha.30 -F publish=false
```

The selected branch supplies the workflow definition; the tag supplies the source
being built. This also lets a workflow change on a PR branch sign an existing tag
before merging. Manual builds upload packages as Actions artifacts. They publish
to GitHub Releases only when `publish` is explicitly enabled. Pushing a `v*` tag
continues to publish automatically after all platform packages succeed.

Apple must return `Accepted`. The workflow also downloads the notary log, staples
the app, and extracts the final archive to verify both signatures, the ticket,
and Gatekeeper assessment. Missing credentials or failed validation fail the
release; there is no ad-hoc fallback. Public submission status and logs are saved
in the `macos-notarization-diagnostics` Actions artifact, including on failure.
A submission that exceeds the 45-minute wait can still be processing at Apple;
the saved submission ID allows its status to be investigated.

The numeric part of the tag becomes `CFBundleShortVersionString`; the release
workflow's run number becomes `CFBundleVersion`. The full prerelease version
remains in the app's own version and the archive's filename. No App Sandbox or
hardened-runtime exception entitlements are added.

Standalone CLI binaries receive notarization tickets but Apple does not support
stapling tickets to them. The app's stapled ticket is checked after extraction;
the CLI may need an online Gatekeeper lookup on first use.

## Checks on a real Mac

Install the downloaded signed build and check launch, input sharing, clipboard
and files, virtual-display creation and capture, and launch at login. The first
upgrade from an ad-hoc identity may require granting permissions again. Upgrade
to a second build with the same bundle ID and signing team to check retention of
Accessibility, Input Monitoring, Screen Recording, and Local Network permissions.
Successful notarization and hosted CI do not prove these hardware interactions.

## References

- [Apple: Developer ID certificates](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/)
- [Apple: App Store Connect API keys](https://developer.apple.com/help/app-store-connect/get-started/app-store-connect-api/)
- [Apple: Customizing notarization](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow)
- [GitHub: Import signing identities on Actions runners](https://docs.github.com/en/actions/how-tos/deploy/deploy-to-third-party-platforms/sign-xcode-applications)
