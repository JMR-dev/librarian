# scripts/

Standalone operational helpers. These are **not** part of the application build,
and the Librarian executable never invokes them — they exist to be picked up by
the separate, user-maintained installer/packaging project.

## `install-ripgrep.ps1`

Provisions [ripgrep](https://github.com/BurntSushi/ripgrep) (`rg.exe`), which
Librarian uses as its recursive-search engine. Intended to run as a **pre-install
step** in the installer for users who don't already have ripgrep.

It resolves the latest `x86_64-pc-windows-msvc` release from GitHub (with a pinned
fallback if GitHub is unreachable), installs `rg.exe` to
`%LOCALAPPDATA%\Programs\ripgrep`, and adds it to the user PATH. It is idempotent:
if ripgrep is already discoverable it does nothing.

```powershell
# Default per-user install (no admin needed):
powershell -ExecutionPolicy Bypass -File .\install-ripgrep.ps1

# Pin a version / install next to the app instead of touching PATH:
powershell -ExecutionPolicy Bypass -File .\install-ripgrep.ps1 -Version 15.1.0 -InstallDir "C:\Program Files\Librarian" -NoPath
```

At runtime Librarian locates `rg.exe` in this order: next to its own executable,
then on `PATH`, then the common winget/scoop/chocolatey locations and the
install dir above. So the installer can either run this script or simply drop
`rg.exe` beside `librarian.exe` for a fully portable install.
