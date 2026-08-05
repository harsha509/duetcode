# Releasing DT Duet to the VS Code Marketplace

Tagging a release publishes the extension automatically — see
[Automated publishing](#automated-publishing). The manual commands below still
work, and are what to reach for when a publish needs to happen off-cycle.

## One-time setup

1. **Azure DevOps org** — the marketplace authenticates through Azure DevOps.
   Sign in at https://dev.azure.com with a Microsoft account and create an
   organization (any name, never shown publicly).

2. **Personal Access Token (PAT)** — in Azure DevOps: avatar → *User
   settings* → *Personal access tokens* → *New Token*:
   - Organization: **All accessible organizations** (required)
   - Expiration: your choice (max 1 year; you'll rotate it)
   - Scopes: *Custom defined* → **Marketplace → Manage**

   Copy the token immediately — it is shown only once.

3. **Publisher** — at https://marketplace.visualstudio.com/manage click
   *Create publisher*. The publisher **ID must exactly match** the
   `"publisher"` field in `package.json` (`harsha509`). Display name and
   description are free-form.

## Automated publishing

`.github/workflows/publish-extension.yml` publishes on the same
`vX.Y.Z` tag that releases the CLI, since the two share a version number. It
installs, compiles, checks the tag against `package.json`, lists what would be
packaged, and publishes.

Two things must exist for it to work:

1. **Repository secret `VSCE_PAT`** — *Settings → Secrets and variables →
   Actions → New repository secret*. Use the Azure DevOps PAT from the one-time
   setup above, with **Marketplace → Manage** scope.

2. **Environment `vscode-marketplace`** — *Settings → Environments → New
   environment*. Add yourself under **Required reviewers**: the job then waits
   for your approval on every run, so no tag publishes to the marketplace
   without you saying yes. Without a reviewer the environment still works, but
   the job runs unattended.

A failed validation can be re-run from *Actions → Publish Extension → Run
workflow*, which skips the tag check and publishes whatever version
`package.json` names — no throwaway tag needed.

The PAT is passed to `vsce` through the environment rather than `--pat`, which
would leave it readable in the runner's process list. Rotate it when it expires
(Azure DevOps PATs last at most a year) by updating the secret; nothing in the
repository needs to change.

## Publishing manually

```bash
cd editors/vscode
npm install
npm run compile

# first time: cache the PAT
npx @vscode/vsce login harsha509

# publish the version currently in package.json
npx @vscode/vsce publish

# or bump-and-publish in one step:
npx @vscode/vsce publish patch    # 0.1.3 -> 0.1.4
npx @vscode/vsce publish minor    # 0.1.3 -> 0.2.0
```

No-login alternative: `npx @vscode/vsce publish -p <PAT>`.

Manual alternative: `npx @vscode/vsce package` and upload the `.vsix` at
https://marketplace.visualstudio.com/manage.

The extension appears at
`https://marketplace.visualstudio.com/items?itemName=harsha509.dt-duet`
within a few minutes of validation, and installs from the Extensions view
search like any other extension.

## Per-release checklist

- [ ] `npm run compile` clean
- [ ] Version bumped in `package.json` (or use `vsce publish patch`)
- [ ] `CHANGELOG.md` has an entry for the new version — it renders as the
      marketplace *Changelog* tab
- [ ] README.md reads well — it *is* the marketplace page
- [ ] `vsce ls` shows only `out/` (no `.map`), `media/`, `package.json`,
      `README.md`, `CHANGELOG.md`, `LICENSE`
- [ ] Commit + tag the release in git (vsce does not touch git)

## Notes

- `vsce publish --pre-release` publishes to the pre-release channel.
- A "Verified publisher" badge is optional domain verification in the
  publisher settings; not required to ship.
- Unpublishing/deprecation is managed from the same manage page.
