# Windows code signing — Azure setup and operations

**Decision:** Robert selected Azure Artifact Signing (formerly Trusted Signing) for
COIN-473 on 2026-11-17. Azure is used only for Windows signing, not application hosting.

The signing-enabled `releases.yml` and `nightly.yml` require valid signatures on
`coincube.exe`, `coincube-spark-bridge.exe`, `coincube-mcp.exe`, and the MSI.
**Implementation is not proof of activation:** Microsoft must finish organization
validation, Robert must configure Azure/GitHub permissions, and an actual Windows
build plus clean-VM acceptance must pass before declaring signed distribution live.

## 1. Owner-controlled onboarding

Robert performs billing, legal identity submission, and live permission changes.
Never put payment information, identity documents, passwords or private keys in
issues, PRs, repository files, or chat.

1. Create a **paid** Azure subscription for COINCUBE. Free, trial and sponsored
   subscriptions cannot use this service.
2. Register `Microsoft.CodeSigning` in the subscription's resource providers.
   Create one Artifact Signing account, in an available region, on **Basic**.
   Record its regional endpoint, for example `https://eus.codesigning.azure.net/`.
3. Assign the human submitting validation the **Artifact Signing Identity Verifier**
   role. Submit **Organization** identity validation using the registered legal
   details of **COINCUBE TECHNOLOGY LLC**, its business identifier and website.
   Follow Microsoft's requests for documentation and contact verification directly
   in its portal. Do not assume approval or an expedited completion date.
4. After validation completes, create one **Public Trust** certificate profile.
   Confirm its validated publisher is `COINCUBE TECHNOLOGY LLC`. Do not use an
   Individual or Private Trust profile to work around an organization mismatch.
5. Create a single-tenant Microsoft Entra app registration/service principal for
   GitHub signing. **Do not create a client secret or client-authentication key.**
   Grant it **Artifact Signing Certificate Profile Signer** only on this profile:

   ```text
   /subscriptions/<subscription>/resourceGroups/<group>/providers/Microsoft.CodeSigning/codeSigningAccounts/<account>/certificateProfiles/<profile>
   ```

   Do not grant subscription Contributor, Owner, or broad resource-group signing
   access to CI.

**Cost:** Basic is **$9.99/account/month**, with **5,000 signatures/month** and
**$0.005 per additional signature**, before tax. Billing starts when the account is
created, not when validation finishes, and the tier charge is not prorated. Four
files per daily build use at most 124 signatures in a 31-day month before
releases/retries. No separate certificate purchase, VM, or Key Vault is needed.
A budget alert is useful but is **not** a spending cap.

## 2. GitHub environments and OIDC

Create both environments in `coincubetech/coincube` **before** enabling federation.
GitHub can otherwise auto-create an environment without the intended protections.

| Environment | Deployment branch/tag policy | Required approval |
|---|---|---|
| `windows-release-signing` | Selected **tags only**, `v*.*`; no branches | Robert must review the exact source SHA/tag before approval |
| `windows-nightly-signing` | Selected **branch only**, `master`; no tags | None, to preserve unattended nightlies |

Use explicit selected-ref rules, not "all branches" or an unverified
"protected branches only" setting. Protect `master`, release-tag creation, and
workflow changes against unreviewed writes; limit environment administrators and
disable protection bypass where supported. Approval must cover the source code,
workflow, dependency changes and tag target, not just the artifact name.
A job on **any** allowed ref that declares one of these environments can request
its token, so an environment name alone is not a workflow-specific security boundary.

Add two federated credentials to the Entra app:

| Field | Value |
|---|---|
| Issuer | `https://token.actions.githubusercontent.com` |
| Audience | `api://AzureADTokenExchange` |
| Release subject | `repo:coincubetech/coincube:environment:windows-release-signing` |
| Nightly subject | `repo:coincubetech/coincube:environment:windows-nightly-signing` |

Use exact subjects, not wildcard/repository-wide trust. Both credentials use the
same certificate profile, within Basic's one-Public-Trust-profile allowance.

Set these **six environment-level Actions variables** in **each** environment.
These are identifiers/configuration, not signing credentials:

```text
AZURE_TENANT_ID
AZURE_CLIENT_ID
AZURE_SUBSCRIPTION_ID
AZURE_ENDPOINT
AZURE_CODE_SIGNING_NAME
AZURE_CERT_PROFILE_NAME
```

Keep the existing build variables (`COINCUBE_API_URL`, passkey settings) at their
existing repository scope; do not move unrelated secrets into the signing environments.

Only the dedicated Windows jobs get `id-token: write`; they get `contents: read`.
`azure/login` exchanges GitHub's short-lived OIDC token. The pinned Artifact Signing
action uses that Azure CLI session, with environment-credential fallback and
executable dependency caching disabled. No `AZURE_CLIENT_SECRET` is used.
OIDC removes a standing credential, but a compromised authorized job can still
request signatures. It does not replace source review or dependency controls.

## 3. Signing and publication gates

Both workflows perform the same sequence:

1. Require all six configuration values before the expensive build. Missing
   configuration fails the Windows job; it never falls back to unsigned output.
2. Build the three executables and copy the bridge into `target/minimal/`.
3. Sign the **explicit three-file list** with SHA-256 and an RFC 3161 timestamp.
4. Verify each executable's Authenticode trust, exact publisher name, timestamp,
   and `signtool verify /pa /all /v /tw` result.
5. Run `cargo wix --no-build` so the signed bytes become the installer payload.
6. Sign and verify the exact MSI path.
7. Administratively extract the MSI. Require exactly one copy of each of the
   three executables, installed as siblings, and verify their signatures again
   **before executing** the companion smoke tests.
8. Upload only after all gates pass. Tagged-release checksum generation waits for
   this Windows job as well as the Linux/macOS builds.

The existing complete-release gate is preserved: a failed Windows leg prevents
publication of the complete tagged bundle. Nightly Linux/macOS artifacts remain
independent. A Windows-less tagged release would need a separately approved
change to the complete-bundle policy, not an unsigned-signing bypass.

Nightly Windows signing accepts only `master`, including manual dispatch. Release
Windows signing accepts version tags and requires the tag to match Cargo's version.
PRs and arbitrary branch dispatches cannot enter these signing jobs.

## 4. Validation and activation checklist

Offline checks, with no Azure access:

```sh
pwsh -NoLogo -NoProfile -File contrib/release/windows/test-signing.ps1
ruby contrib/release/windows/test-workflows.rb
actionlint -shellcheck= .github/workflows/{releases,nightly,main}.yml
```

The PowerShell tests mock OS trust APIs/SignTool to exercise failure handling;
they do not establish that a real certificate, timestamp or Azure login works.
They also reject a real unsigned fixture when run on Windows. Main CI runs the
PowerShell checks on Windows and workflow checks on Linux.

After onboarding, Robert authorizes an initial `master` nightly. Confirm its exact
source SHA, successful OIDC exchange, all signatures and extracted payload checks.
Retain the run URL and artifact hashes as activation evidence. Then on a clean,
updated Windows VM with no repository checkout:

- Download the MSI through a browser, preserving its Internet origin.
- Verify the release checksum/GPG signature for a tagged release, plus the MSI's
  Digital Signatures details and publisher **COINCUBE TECHNOLOGY LLC**.
- Install; verify all three installed executable signatures and publisher names.
- Launch the app, exercise Spark startup and local MCP pairing/STDIO.
- Record Windows version, artifact hash, publisher, install and launch results.

SmartScreen may still report an unrecognized app while reputation develops.
**Do not promise warning-free downloads.** Unknown publisher, invalid signature,
missing timestamp, or broken companion startup is a failed acceptance check.

## 5. Renewal, failure and incident handling

Monitor identity-validation expiry notices. Microsoft sends reminders beginning
60 days before expiry; if validation lapses, certificate renewal and new signing
stop. Keep a named owner and renew validation before expiry. Short-lived signing
certificates rotate automatically; correctly timestamped signatures can remain
valid after the signing certificate expires.

For configuration, login, signing or verification failures, diagnose the failing
gate and rerun only after fixing its cause. Do not add an unsigned fallback.
If validation cannot complete, return to Robert for a launch/provider decision,
including whether to hold Windows distribution.

If CI or the signing identity is compromised, Robert should disable the federated
credentials/role assignment, stop affected publication, preserve build hashes and
audit records, and contact Microsoft about affected signatures. Never assume
deleting a profile revokes previously issued signatures.

## Official references

- [Service overview](https://learn.microsoft.com/en-us/azure/artifact-signing/)
- [Pricing and quotas](https://learn.microsoft.com/en-us/azure/artifact-signing/how-to-change-sku)
- [FAQ: subscription, validation, billing and certificate lifecycle](https://learn.microsoft.com/en-us/azure/artifact-signing/faq)
- [GitHub Actions OIDC](https://learn.microsoft.com/en-us/azure/developer/github/connect-from-azure-openid-connect)
- [Artifact Signing action](https://github.com/Azure/artifact-signing-action)
