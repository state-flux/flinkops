# Release process

中文: [release.md](release.md)

Every FlinkOps release is produced by **one tag and one pipeline**: GitHub Actions builds the binaries
on native runners, `scripts/dist.py` packs them into archives and installers, `scripts/release_notes.py`
writes the release notes, and `gh` uploads everything to a single GitHub Release.

```
git tag -a v0.1.0 -m "FlinkOps 0.1.0" && git push origin v0.1.0
        │
        ├─ verify    version consistency (tag = Cargo.toml = Chart.yaml = apps/web/package.json) + cargo test
        ├─ dist      Linux x86_64/arm64, macOS x86_64/arm64, Windows x86_64 → tar.gz / deb / zip / setup.exe
        ├─ image     ghcr.io/<owner>/flinkops (linux/amd64 + linux/arm64, distroless)
        ├─ chart     helm package → ghcr.io/<owner>/charts (OCI)
        └─ release   RELEASE_NOTES.md + SHA256SUMS.txt + all artifacts
```

## 1. One source of truth for the version

Align these four places to the same `X.Y.Z` (CI `verify` compares each one and fails on mismatch):

| File | Field |
|---|---|
| `Cargo.toml` | `[workspace.package].version` |
| `deploy/helm/flinkops/Chart.yaml` | `version` and `appVersion` |
| `apps/web/package.json` | `version` |

Add a `## [X.Y.Z] - YYYY-MM-DD` section to `CHANGELOG.md`; the release notes reuse it verbatim.
A version containing `-` (e.g. `0.2.0-rc.1`) is treated as a pre-release: no `latest` image tag,
Release marked pre-release.

## 2. Local pre-flight (recommended)

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cd apps/web && npm ci && npm run lint && npm test && npm run build
```

Produce a local package to exercise the packager (nothing is uploaded):

```bash
make dist FORMATS=tar.gz,deb     # Linux (also emits src.tar.gz / src.zip)
make dist FORMATS=zip,setup      # Windows (needs makensis, see below)
make dist-src                    # source archives only
make notes                       # writes dist/RELEASE_NOTES.md
```

## 3. Artifact matrix

| Platform | Artifact | Install |
|---|---|---|
| Linux x86_64 (glibc ≥ 2.34) | `flinkops-<ver>-x86_64-unknown-linux-gnu.tar.gz` | `tar -xzf … && ./start.sh` |
| Linux arm64 (glibc ≥ 2.34) | `flinkops-<ver>-aarch64-unknown-linux-gnu.tar.gz` | same |
| Debian / Ubuntu amd64 / arm64 | `flinkops_<ver>_amd64.deb`, `flinkops_<ver>_arm64.deb` | `dpkg -i`, systemd unit |
| Linux (amd64 / arm64) | `flinkops-<ver>-<triple>.AppImage` | `chmod +x` and run — no install |
| macOS Intel / Apple Silicon | `flinkops-<ver>-<triple>.tar.gz` | `tar -xzf … && ./start.sh` |
| Windows x86_64 | `flinkops-<ver>-x86_64-pc-windows-msvc.zip` | unzip and double-click `flinkops.exe` |
| Windows x86_64 | `flinkops-<ver>-x86_64-pc-windows-msvc-setup.exe` | double-click; per-user install, no admin/UAC |
| Source (any platform) | `flinkops-<ver>-src.tar.gz`, `flinkops-<ver>-src.zip` | unpack, `cargo build --release` |
| Container | `ghcr.io/state-flux/flinkops:<ver>` | `docker run` / Helm |
| Helm | `flinkops-<ver>.tgz` (OCI) | `helm install … oci://ghcr.io/state-flux/charts/flinkops` |

**Every artifact ships with a matching `.sha256`** (sha256sum format, so `sha256sum -c <file>.sha256` works);
`SHA256SUMS.txt` additionally aggregates them (excluding the sidecars themselves).

### Source archives

`src.tar.gz` and `src.zip` are the same file set in two formats (`--formats src-tar.gz,src-zip`):

- the manifest comes from `git ls-files --cached --others --exclude-standard` — **tracked files plus untracked,
  non-ignored files**, so `target/`, `dist/`, `node_modules/`, `data/`, `.secrets/` never enter the archive;
- in CI (run on a tag) the manifest is exactly that tag's content; a local run also picks up new but
  uncommitted files, which is handy for self-testing;
- `Cargo.lock` and `apps/web/package-lock.json` are included, so `cargo build` and `npm ci` reproduce the build;
- layout is `flinkops-<ver>-src/`, mtimes come from `SOURCE_DATE_EPOCH` (defaults to the commit date), and the
  tar.gz and zip contain the same paths.

GitHub also attaches its own "Source code (zip/tar.gz)" to every release; ours additionally shares the packager's
naming/prefix and is asserted by the `release` job.

```bash
tar -xzf flinkops-0.1.0-src.tar.gz
cd flinkops-0.1.0-src
cd apps/web && npm ci && npm run build && cd -
cargo build --release --bins -p flinkops-api -p flinkops-cli -p flinkops-mcp
FLINKOPS_WEB_DIR=apps/web/dist ./target/release/flinkops
```

Archives and the installer share **one layout**, and both are ready to run right after unpacking/installing:

```
flinkops[.exe]        control plane (axum) + Web UI hosting; double-click to start
flinkopsctl[.exe]     ops CLI
flinkops-mcp[.exe]    MCP server (stdio / Streamable HTTP)
web/                  Vue 3 build output (index.html + assets)
.env                  zero-config: bind 127.0.0.1:8080, web=./web, data=./data, auto-open browser
start.cmd / start.sh  optional launcher (same as double-clicking flinkops.exe)
data/                 created on first run (SQLite metadata)
install.ps1 / install.sh  optional system install (no admin needed / PREFIX)
uninstall.ps1         uninstaller for the Windows portable build
systemd/flinkops.service  optional systemd unit for Linux
LICENSE / NOTICE / README.md / VERSION / build-info.json
```

Behaviour:

- `.env` is loaded **before** any other config read; relative `FLINKOPS_WEB_DIR` / `FLINKOPS_STORE` resolve
  against the **exe directory**, so double-clicking works no matter what the working directory is;
- a missing kubeconfig is not fatal — cluster connection failures are warnings and the UI still serves;
- the console prints `open: http://127.0.0.1:8080` plus the default account; `FLINKOPS_OPEN_BROWSER=1` opens it.

## 4. The packager: `scripts/dist.py`

Pure-stdlib, runs on Windows / Linux / macOS; it is what `make dist` calls:

```bash
python scripts/dist.py --version 0.1.0 --target x86_64-unknown-linux-gnu --formats tar.gz,deb
python scripts/dist.py --version 0.1.0 --target x86_64-pc-windows-msvc --formats zip,setup
python scripts/dist.py --version 0.1.0 --formats src-tar.gz,src-zip     # source, platform-independent
python scripts/dist.py --list-targets
python scripts/dist.py --list-formats
```

| Flag | Meaning |
|---|---|
| `--version` | version; defaults to `Cargo.toml` |
| `--target` | target triple; defaults to the host |
| `--bin-dir` | release binaries; defaults to `target/<target>/release`, falls back to `target/release` |
| `--web-dir` | SPA output; defaults to `apps/web/dist` (missing → hard error, so no UI-less release) |
| `--formats` | `tar.gz` / `zip` / `deb` / `appimage` / `appdir` / `setup` / `src-tar.gz` / `src-zip`, per-platform default; source formats are target-independent |
| `--out` | output directory, default `dist/` |
| `--commit` | override the commit recorded in `build-info.json` |

It refreshes `dist/SHA256SUMS.txt` for every artifact in the output directory; the CI `release` job
recomputes an authoritative copy. Archives are reproducible: `SOURCE_DATE_EPOCH`
(defaults to `git log -1 --format=%ct`) drives tar mtimes and zip timestamps.

### What the installers actually do

- **Windows `setup.exe` (NSIS)**: **per-user install — no admin, no UAC**, into
  `%LOCALAPPDATA%\Programs\FlinkOps`; writes `.env` and `data\`, creates Start Menu and Desktop shortcuts
  (with a working directory), optionally appends the install dir to the **user** PATH; registers under
  `HKCU\...\Uninstall\FlinkOps` and keeps `data\` on uninstall (GUI asks, silent keeps).
- **Windows `zip`**: unpack and run — double-click `flinkops.exe` (or `start.cmd`); `install.ps1` is optional.
- **Linux / macOS `tar.gz`**: unpack and run — `./start.sh` or `./flinkops`; `install.sh` optionally installs into
  `PREFIX/bin` and puts `web/` at `PREFIX/lib/flinkops/web`.
- **`.deb`**: installs `/usr/bin/{flinkops,flinkopsctl,flinkops-mcp}`, `/usr/lib/flinkops/web`,
  `/lib/systemd/system/flinkops.service`; `postinst` creates the `flinkops` user and
  `/etc/flinkops/flinkops.env` (conffile); `prerm` stops the unit; `postrm purge` removes data.

### AppImage (single-file, no install)

`--formats appimage` produces `flinkops-<ver>-<triple>.AppImage` for amd64 and arm64:

1. build an AppDir (`usr/bin/*`, `usr/share/flinkops/web/`, `AppRun`, `.desktop`, 256×256 icon);
2. `mksquashfs` it (zstd → gzip → xz fallbacks);
3. prepend the official `runtime-<arch>` and `chmod +x`.

Requirements (the CI job installs them):

```bash
sudo apt-get install -y squashfs-tools
mkdir -p .tools/appimage
curl -fsSL -o .tools/appimage/runtime-x86_64 \
  https://github.com/AppImage/AppImageKit/releases/download/continuous/runtime-x86_64
chmod +x .tools/appimage/runtime-x86_64        # use runtime-aarch64 for arm64
python scripts/dist.py --target x86_64-unknown-linux-gnu --formats tar.gz,deb,appimage
```

`APPIMAGE_RUNTIME=/path/to/runtime` overrides discovery. Without squashfs-tools,
`--formats appdir` still emits `<…>.AppDir.tar.gz` with the same tree for debugging or as a fallback.

Run: `chmod +x flinkops-*.AppImage && ./flinkops-*.AppImage`; on FUSE-less systems set
`APPIMAGE_EXTRACT_AND_RUN=1`.

### makensis for the Windows installer

`dist.py --formats setup` looks at `MAKENSIS`, then `.tools/nsis/makensis.exe`, then `PATH`.
Without NSIS installed, use the bundled helper (no admin needed; `.tools/` is git-ignored):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch-nsis.ps1
$env:MAKENSIS = "$PWD\.tools\nsis\makensis.exe"
python scripts/dist.py --target x86_64-pc-windows-msvc --formats zip,setup
```

The helper runs `winget download NSIS.NSIS` and unpacks the official installer with `/S /D=<dir>`.
`choco install nsis -y` or `winget install NSIS.NSIS` work too; the CI Windows job uses choco and
exports `MAKENSIS`.

## 4.5 Cross-compiling (Linux artifacts straight from your machine)

The TLS backend is pure Rust now (`crates/flinkops-tls`: `rustls` + `rustls-rustcrypto`), so the only C code
left in the tree is bundled SQLite — no cmake, no nasm, no target OpenSSL:

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild          # zig cc acts as the cross C compiler

cargo zigbuild --release --locked --target x86_64-unknown-linux-musl \
  --bins -p flinkops-api -p flinkops-cli -p flinkops-mcp
# → target/x86_64-unknown-linux-musl/release/{flinkops,flinkopsctl,flinkops-mcp} (static, musl)
```

- `zig cc` only builds bundled SQLite; the musl target links statically by default;
- for aarch64 use `aarch64-unknown-linux-musl`;
- macOS still needs the Apple SDK (osxcross or a mac runner) — same as Go with cgo;
- no zig? use `cross build --target …` (Docker) or the native CI matrix above;
- packaging: `python scripts/dist.py --bin-dir target/x86_64-unknown-linux-musl/release \
  --target x86_64-unknown-linux-musl --formats tar.gz` (no `.deb` for musl).

## 5. Trigger and observe

```bash
git tag -a v0.1.0 -m "FlinkOps 0.1.0"
git push origin v0.1.0        # pushing the tag releases

gh run watch                  # or: gh run list --workflow=release
gh release view v0.1.0
```

Manual trigger: `gh workflow run release -f tag=v0.1.0`.

The `release` job **asserts every artifact exists** (11 patterns: six platform packages, two source archives,
two debs, one chart — a missing platform fails the release), then:

1. renders `RELEASE_NOTES.md` via `scripts/release_notes.py` (changelog + download table + install commands + verification);
2. recomputes `SHA256SUMS.txt` over everything in `dist/`;
3. `gh release create --verify-tag` (or `upload --clobber` + `edit` when it already exists),
   with `--latest` for stable and `--prerelease` for pre-releases.

## 6. Post-release verification

```bash
sha256sum -c SHA256SUMS.txt --ignore-missing

sudo dpkg -i flinkops_0.1.0_amd64.deb
sudoedit /etc/flinkops/flinkops.env      # FLINKOPS_CLUSTERS
sudo systemctl start flinkops && curl -fsS localhost:8080/healthz

# Windows: run setup.exe → bin\flinkops.exe → http://127.0.0.1:8080 (admin/admin)

docker run --rm ghcr.io/state-flux/flinkops:0.1.0 /app/bin/flinkopsctl --version
```

## 7. When something goes wrong

| Symptom | Action |
|---|---|
| `verify` reports a version mismatch | align `Cargo.toml` / `Chart.yaml` / `package.json`, re-tag |
| one `dist` leg fails | `fail-fast: false` keeps the rest; fix, re-run that job, or `gh release upload --clobber` |
| `release` reports a missing artifact | get that platform green first — a half release is worse than a failed one |
| need to re-cut the same tag | `gh release delete v0.1.0 -y --cleanup-tag`, then push the tag again |
| Windows SmartScreen warning | expected: packages are unsigned; verify with `SHA256SUMS.txt` |
| macOS says the archive is damaged | not notarized: `xattr -dr com.apple.quarantine flinkops-0.1.0-aarch64-apple-darwin.tar.gz` |

## 8. Not covered yet

- `.rpm`, `AppImage`, macOS `.dmg` + notarization, Windows code signing — add another `--formats`
  branch to `dist.py` when needed.
- Automatic Homebrew / Scoop / winget manifest publishing.
