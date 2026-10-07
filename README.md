# Project Life

Local file history and recovery for your projects — especially when an AI coding agent, editor, or script deletes or overwrites your work.

Project Life watches folders you choose. It saves one initial copy of eligible files, then stores the **complete contents of changed files only**, with content deduplication and a timeline of the project tree. History is kept outside the project, on your own disk.

**No cloud account. No telemetry. No uploads.**

## What you can do

- Protect code, documents, and creative-project files using selectable file types and exclusion rules.
- Browse file versions and the project tree at a saved moment.
- Restore a file, a folder, or a project to a separate location.
- Repair missing files without overwriting existing work.
- Keep background protection running when the application window is closed.
- See protection status, storage warnings, and notification history.
- Export and import history, and use the command-line interface.

## Download

Get the packages from [GitHub Releases](https://github.com/oxunjonuz/project-life/releases).

| Platform | Package | Status for this release |
| --- | --- | --- |
| macOS Apple Silicon | `ProjectLife-macos-arm64.tar.gz` | Built; this release still needs native macOS validation |
| Windows 10/11 x86-64 | `ProjectLife-windows-x86_64.zip` | Built; not yet run on Windows |
| Linux ARM64 | Portable `.tar.gz` or Debian `.deb` | Executed and tested on Linux |

Windows requires Microsoft WebView2 Runtime. Linux requires GTK 3 and WebKitGTK; see [platform requirements](docs/PLATFORMS.md). Linux x86-64 binaries are not included in this release.

macOS and Windows packages are not signed with developer certificates. Checksums are provided with the release.

## Getting started

1. Install or extract the package for your system and open Project Life.
2. Choose an archive folder, preferably on a separate disk.
3. Add the project folders and file types you want to protect.
4. Start background protection and check its status.
5. Use History to preview and restore saved versions. Try recovery on a small test folder first.

See [installation and usage](docs/INSTALL_AND_USE.md), [desktop application](docs/APP.md), and [recovery instructions](docs/RECOVERY.md).

## Command line

```sh
pl init-archive ~/pl-archive
pl add ~/work/my-project
pl daemon start
pl status
pl tree my-project --at "1h ago"
pl restore my-project --at "1h ago" --to ./recovered
```

`pl` and `projectlife` use the same core. On Windows, use `pl.exe`.

## Important limits

The program records **observed versions**, not every individual write. The default observation interval is five seconds; a version created and removed between observations may not be saved. Protection must be running, and the archive must be writable with enough free space.

The archive is not encrypted and does not back itself up. Keep another copy of important archives. See [limitations](docs/LIMITATIONS.md) and [security](docs/SECURITY.md).

## Build and test

```sh
cargo build --release
(cd app && cargo build --release)
cargo test --release
(cd app && cargo test --release)
```

Platform build instructions: [docs/PLATFORMS.md](docs/PLATFORMS.md). Archive format: [docs/STORAGE_FORMAT.md](docs/STORAGE_FORMAT.md).

## License and contact

[MIT](LICENSE). Microsoft WebView2 components retain their third-party license.

Created by Oxunjon Ubaydllayev with AIODAM. Contact: [oxunjonub@gmail.com](mailto:oxunjonub@gmail.com).
