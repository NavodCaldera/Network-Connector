# Publishing this repository to GitHub

Do this once, on the i7, from the project folder.

## 1. Check that nothing secret is included

Secrets live in `%LOCALAPPDATA%\NetworkConnector`, not in this folder. Still, check:

```powershell
git init
git add -A
git status --short          # review the list
git ls-files | Select-String -Pattern "\.db$|\.key$|\.crt$|\.token$|^\.env$|worker\.json|\.venv"
```

The last command must print **nothing**.

## 2. Set the repository URL in the worker setup script

Edit `scripts/setup-worker.ps1` and set:

```powershell
$DefaultRepoUrl = "https://github.com/NavodCaldera/Network-Connector.git"
```

## 3. Create the repository and push

Using the GitHub CLI (`winget install -e --id GitHub.cli`, then `gh auth login`):

```powershell
git branch -M main
git commit -m "Initial release of Network Connector 0.1.0"
gh repo create network-connector --private --source . --remote origin --push
```

Choose `--public` instead of `--private` if you want the i3 to download without signing in.
Nothing in the repository is secret, but a public repo is visible to everyone.

Without the GitHub CLI: create an empty repository at <https://github.com/new> (no README), then:

```powershell
git remote add origin https://github.com/NavodCaldera/Network-Connector.git
git push -u origin main
```

## 4. Publish the first release tag

Workers install only tagged releases:

```powershell
git tag -a v0.1.0 -m "Network Connector 0.1.0"
git push origin v0.1.0
```

## 5. Recommended GitHub settings

- **Settings → Branches → Add rule** for `main`: require pull requests (or at least block force-pushes).
- **Settings → Tags → protect `v*`**, so only you can create release tags.
- Enable 2-factor authentication on your account.
- Optional: sign tags (`git tag -s`) and update workers with `nc worker update --verify-signature`.
