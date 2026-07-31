# Releasing DT Duet to the VS Code Marketplace

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

## Publishing a release

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
