# 发版流程（Release）

English: [release.en.md](release.en.md)

FlinkOps 的发行由 **一个 tag 触发一条流水线**：GitHub Actions 在原生 runner 上编译各平台二进制，
`scripts/dist.py` 打成统一的压缩包 / 安装包，`scripts/release_notes.py` 生成发行说明，
最后用 `gh` 把全部产物挂到同一个 GitHub Release。

```
git tag -a v0.1.0 -m "FlinkOps 0.1.0" && git push origin v0.1.0
        │
        ├─ verify    版本一致性（tag = Cargo.toml = Chart.yaml = apps/web/package.json）+ cargo test
        ├─ dist      Linux x86_64/arm64、macOS x86_64/arm64、Windows x86_64 → tar.gz / deb / zip / setup.exe
        ├─ image     ghcr.io/<owner>/flinkops（linux/amd64 + linux/arm64，distroless）
        ├─ chart     helm package → ghcr.io/<owner>/charts（OCI）
        └─ release   RELEASE_NOTES.md + SHA256SUMS.txt + 全部产物上传
```

## 1. 版本号只有一个来源

发版前把下面四处对齐到同一个 `X.Y.Z`（CI 的 `verify` 会逐个比对，不一致直接失败）：

| 文件 | 字段 |
|---|---|
| `Cargo.toml` | `[workspace.package].version` |
| `deploy/helm/flinkops/Chart.yaml` | `version` 与 `appVersion` |
| `apps/web/package.json` | `version` |

`CHANGELOG.md` 里补一节 `## [X.Y.Z] - YYYY-MM-DD`，发行说明会直接引用这一节。
版本号里带 `-`（如 `0.2.0-rc.1`）会被识别为预发布：镜像不打 `latest`，Release 标 `pre-release`。

## 2. 本地自测（可选但推荐）

```bash
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cd apps/web && npm ci && npm run lint && npm test && npm run build
```

打一份本机发行包验证打包器（不上传）：

```bash
make dist FORMATS=tar.gz,deb     # Linux（同时产出 src.tar.gz / src.zip）
make dist FORMATS=zip,setup      # Windows（需要 makensis，见下文）
make dist-src                    # 只打源码包
make notes                       # 生成 dist/RELEASE_NOTES.md
```

## 3. 产物矩阵

| 平台 | 产物 | 安装方式 |
|---|---|---|
| Linux x86_64 (glibc ≥ 2.34) | `flinkops-<ver>-x86_64-unknown-linux-gnu.tar.gz` | `tar -xzf … && ./start.sh` |
| Linux arm64 (glibc ≥ 2.34) | `flinkops-<ver>-aarch64-unknown-linux-gnu.tar.gz` | 同上 |
| Debian / Ubuntu amd64 | `flinkops_<ver>_amd64.deb` | `dpkg -i`，systemd unit `flinkops.service` |
| Debian / Ubuntu arm64 | `flinkops_<ver>_arm64.deb` | 同上 |
| Linux 通用（amd64 / arm64） | `flinkops-<ver>-<triple>.AppImage` | `chmod +x` 后直接运行，免安装 |
| macOS Intel / Apple Silicon | `flinkops-<ver>-<triple>.tar.gz` | `tar -xzf … && ./start.sh` |
| Windows x86_64 | `flinkops-<ver>-x86_64-pc-windows-msvc.zip` | 解压双击 `flinkops.exe`，无需安装 |
| Windows x86_64 | `flinkops-<ver>-x86_64-pc-windows-msvc-setup.exe` | 双击安装（按用户，免管理员/UAC） |
| 源码（任意平台） | `flinkops-<ver>-src.tar.gz`、`flinkops-<ver>-src.zip` | 解压后 `cargo build --release` |
| 容器 | `ghcr.io/state-flux/flinkops:<ver>` | `docker run` / Helm |
| Helm | `flinkops-<ver>.tgz`（OCI） | `helm install … oci://ghcr.io/state-flux/charts/flinkops` |

**每个产物都带一个同名 `.sha256`**（`sha256sum` 格式，可直接 `sha256sum -c <file>.sha256`），
另外汇总一份 `SHA256SUMS.txt`（不含 `.sha256` 自身）。

### 源码包

`src.tar.gz` / `src.zip` 是同一份清单的两种压缩格式（`--formats src-tar.gz,src-zip`）：

- 清单来自 `git ls-files --cached --others --exclude-standard`：**受版本控制的文件 + 未被 `.gitignore` 命中的新文件**，
  因此 `target/`、`dist/`、`node_modules/`、`data/`、`.secrets/` 不会进包；
- 打 tag 后跑 CI 时，清单就等于该 tag 的内容；本地打出来的包会带上尚未提交但未被忽略的新文件（便于自测）；
- `Cargo.lock`、`apps/web/package-lock.json` 都在包内，`cargo build --offline` 与 `npm ci` 可复现构建；
- 归档结构 `flinkops-<ver>-src/`，mtime 由 `SOURCE_DATE_EPOCH`（缺省取 commit 时间）决定，tar.gz 与 zip 内容一致。

GitHub 自己的 "Source code (zip/tar.gz)" 也会出现在每个 Release 上；我们这份额外保证「与打包器同一套命名/前缀」
和「可在 CI 里做产物齐全性断言」，`release` job 会校验它存在。

```bash
tar -xzf flinkops-0.1.0-src.tar.gz
cd flinkops-0.1.0-src
cd apps/web && npm ci && npm run build && cd -
cargo build --release --bins -p flinkops-api -p flinkops-cli -p flinkops-mcp
FLINKOPS_WEB_DIR=apps/web/dist ./target/release/flinkops
```

压缩包与安装包是**同一套布局**，解压/安装后直接双击就能跑（不需要再"安装"一次）：

```
flinkops[-.exe]        控制面（axum）+ Web UI 托管，双击即启动
flinkopsctl[.exe]      运维 CLI
flinkops-mcp[.exe]     MCP server（stdio / Streamable HTTP）
web/                   Vue 3 构建产物（index.html + assets）
.env                   零配置：监听 127.0.0.1:8080、web=./web、data=./data、自动开浏览器
start.cmd / start.sh   可选启动器（Windows 下等价于双击 flinkops.exe）
data/                  运行后自动生成（SQLite 元数据库）
install.ps1 / install.sh  可选：装到系统路径（免管理员 / PREFIX）
uninstall.ps1          Windows 便携版的卸载脚本
systemd/flinkops.service   Linux 的 systemd unit（可选）
LICENSE / NOTICE / README.md / VERSION / build-info.json
```

行为约定：

- `.env` 在读取任何配置**之前**加载；相对的 `FLINKOPS_WEB_DIR` / `FLINKOPS_STORE` 以 **exe 所在目录**为基准，
  因此双击启动（CWD 可能是任意目录）也能正确找到 `web/` 并把数据写进旁边的 `data/`；
- 没装 kubeconfig 也能起来：集群连接失败只 warning，静态页面与只读视图照常可用；
- 启动后控制台打印 `open: http://127.0.0.1:8080` 与默认账号，`FLINKOPS_OPEN_BROWSER=1` 时自动开浏览器。

## 4. 打包器 `scripts/dist.py`

跨平台纯标准库脚本（Windows / Linux / macOS 都能跑，CI 与本机共用），也是 `make dist` 的实现：

```bash
python scripts/dist.py --version 0.1.0 --target x86_64-unknown-linux-gnu --formats tar.gz,deb
python scripts/dist.py --version 0.1.0 --target x86_64-pc-windows-msvc --formats zip,setup
python scripts/dist.py --version 0.1.0 --formats src-tar.gz,src-zip     # 源码包，与平台无关
python scripts/dist.py --list-targets
python scripts/dist.py --list-formats
```

| 参数 | 说明 |
|---|---|
| `--version` | 版本号，缺省读 `Cargo.toml` |
| `--target` | 目标三元组，缺省按本机推断 |
| `--bin-dir` | release 二进制目录，缺省 `target/<target>/release`，不存在时回落 `target/release` |
| `--web-dir` | 前端产物目录，缺省 `apps/web/dist`（缺失则报错，避免发行包里没有 UI） |
| `--formats` | `tar.gz` / `zip` / `deb` / `appimage` / `appdir` / `setup` / `src-tar.gz` / `src-zip`，按目标平台默认取值；源码格式与目标平台无关 |
| `--out` | 输出目录，缺省 `dist/` |
| `--commit` | 覆盖 `build-info.json` 里的 commit（CI 用 `github.sha`） |

脚本会顺带刷新 `dist/SHA256SUMS.txt`（同一目录多平台产物汇总，CI 的 release job 会重算一份权威版本）。
构建可复现：`SOURCE_DATE_EPOCH`（缺省取 `git log -1 --format=%ct`）决定归档内 mtime 与 zip 时间戳。

### 安装包里都改了什么

- **Windows `setup.exe`（NSIS）**：**按当前用户安装，不需要管理员、不弹 UAC**，装到
  `%LOCALAPPDATA%\Programs\FlinkOps`；生成 `.env`、`data\`、开始菜单与桌面快捷方式（快捷方式带工作目录），
  可选把安装目录写进**用户** PATH；卸载项注册在 `HKCU\...\Uninstall\FlinkOps`，
  卸载时保留 `data\`（图形卸载会问一次，静默卸载不动数据）。
- **Windows `zip`**：解压即可用——双击 `flinkops.exe`（或 `start.cmd`）启动；`install.ps1` 只是可选增强。
- **Linux / macOS `tar.gz`**：解压即可用——`./start.sh` 或 `./flinkops`；`install.sh` 可选装到 `PREFIX/bin`
  （默认 `/usr/local`），并把 `web/` 放到 `PREFIX/lib/flinkops/web`。
- **`.deb`**：装到 `/usr/bin/{flinkops,flinkopsctl,flinkops-mcp}`、`/usr/lib/flinkops/web`、
  `/lib/systemd/system/flinkops.service`；`postinst` 建 `flinkops` 系统用户、生成
  `/etc/flinkops/flinkops.env`（conffile，升级不覆盖）；`prerm` 停服务；`postrm purge` 清数据。

### AppImage（Linux 免安装单文件）

`--formats appimage` 生成 `flinkops-<ver>-<triple>.AppImage`（amd64 / arm64 都支持）：

1. 先摊出 AppDir：`usr/bin/{flinkops,flinkopsctl,flinkops-mcp}`、`usr/share/flinkops/web/`、
   `AppRun`（把 web / `~/.local/share/flinkops` / 自动开浏览器配好）、`flinkops.desktop`、256×256 图标；
2. `mksquashfs` 打成 squashfs（依次尝试 zstd → gzip → xz）；
3. 拼在官方 runtime 前面：`runtime-<arch>` + `flinkops.squashfs` → `.AppImage`，并 `chmod +x`。

依赖（CI 已自动装好）：

```bash
sudo apt-get install -y squashfs-tools
mkdir -p .tools/appimage
curl -fsSL -o .tools/appimage/runtime-x86_64 \
  https://github.com/AppImage/AppImageKit/releases/download/continuous/runtime-x86_64
chmod +x .tools/appimage/runtime-x86_64        # arm64 换成 runtime-aarch64
python scripts/dist.py --target x86_64-unknown-linux-gnu --formats tar.gz,deb,appimage
```

也可以 `APPIMAGE_RUNTIME=/path/to/runtime` 显式指定 runtime。没有 squashfs-tools 时，
`--formats appdir` 仍会产出 `<…>.AppDir.tar.gz`（同样的目录树，供排查或降级分发）。

运行：`chmod +x flinkops-*.AppImage && ./flinkops-*.AppImage`；无 FUSE 的环境加
`APPIMAGE_EXTRACT_AND_RUN=1`。

### 生成 Windows 安装包需要的 makensis

`dist.py --formats setup` 会依次找 `MAKENSIS` 环境变量 → `.tools/nsis/makensis.exe` → `PATH`。
本机没有 NSIS 时，用仓库里的脚本把它装到 `.tools/nsis`（免管理员，`.tools/` 已被 `.gitignore` 忽略）：

```powershell
powershell -ExecutionPolicy Bypass -File scripts\fetch-nsis.ps1
$env:MAKENSIS = "$PWD\.tools\nsis\makensis.exe"
python scripts/dist.py --target x86_64-pc-windows-msvc --formats zip,setup
```

脚本做的事：`winget download NSIS.NSIS` 取官方安装包，再以 `/S /D=<目录>` 静默解到 `.tools/nsis`。
也可以用包管理器：`choco install nsis -y` 或 `winget install NSIS.NSIS`。
CI 的 Windows job 用 `choco install nsis` 并写入 `MAKENSIS`。

## 4.5 交叉编译（本机直接出 Linux 包）

TLS 后端已经是纯 Rust（`crates/flinkops-tls`：`rustls` + `rustls-rustcrypto`），
依赖树里只剩 bundled SQLite 一处 C 代码，所以交叉编译不再需要 cmake / nasm / 目标平台 OpenSSL：

```bash
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo install --locked cargo-zigbuild          # 用 zig cc 当交叉 C 编译器

cargo zigbuild --release --locked --target x86_64-unknown-linux-musl \
  --bins -p flinkops-api -p flinkops-cli -p flinkops-mcp
# → target/x86_64-unknown-linux-musl/release/{flinkops,flinkopsctl,flinkops-mcp}（静态，musl）
```

- `zig cc` 只用来编译 bundled SQLite；`-C target-feature=+crt-static` 由 musl 目标自带；
- aarch64 把 target 换成 `aarch64-unknown-linux-musl` 即可；
- macOS 目标仍需要 Apple SDK（osxcross，或直接用 mac runner）——这点和 Go 也一样；
- 不想装 zig：`cross build --target …`（需要 Docker），或直接用本文的 CI 原生矩阵；
- 打包：`python scripts/dist.py --bin-dir target/x86_64-unknown-linux-musl/release \
  --target x86_64-unknown-linux-musl --formats tar.gz`（musl 目标不发 `.deb`，因为 `.deb` 依赖 glibc 语义）。

## 5. 触发与观察

```bash
git tag -a v0.1.0 -m "FlinkOps 0.1.0"
git push origin v0.1.0        # 推送 tag 即发版

gh run watch                  # 或 gh run list --workflow=release
gh release view v0.1.0        # 产物列表
```

也可以手动触发：`gh workflow run release -f tag=v0.1.0`。

`release` job 会**校验产物齐全**（15 类：Linux tar.gz / deb / AppImage × 2 架构、macOS tar.gz × 2、Windows zip + setup.exe、源码 tar.gz/zip、chart，以及各类 `.sha256`；少一个就失败），然后：

1. 用 `scripts/release_notes.py` 生成 `RELEASE_NOTES.md`（变更摘要 + 下载表 + 安装命令 + 校验方式）；
2. 对 `dist/` 下全部附属产物重算 `SHA256SUMS.txt`；
3. `gh release create --verify-tag`（已存在则 `gh release upload --clobber` 后 `gh release edit`）；
   正式版加 `--latest`，预发布加 `--prerelease`。

## 5.5 本地准备上传包

要手动往 Release 页面拖文件时，先让脚本把清单整理好（附件清单 + 上传步骤 + 一个 zip）：

```bash
python scripts/dist.py --version 0.1.0 --formats zip,setup          # 本机能编的部分
python scripts/dist.py --version 0.1.0 --formats src-tar.gz,src-zip
python scripts/release_notes.py --version 0.1.0 --dist dist
python scripts/upload_bundle.py --version 0.1.0 --dist dist
# → dist/MANIFEST.md       附件清单（文件 / 大小 / SHA-256 / 平台）
# → dist/UPLOAD.md         上传步骤（拖哪些、正文贴哪个、怎么标 latest）
# → dist/flinkops-0.1.0-upload-bundle.zip   以上 + 全部附件，方便搬机器
```

清单里的产物名同样来自 `dist.py`，本地编不出来的平台（Linux/macOS）会单独列成「待 CI 补齐」，
避免把半套产物当成全套发出去。CI 上也可以跑 `make bundle` 复核。

## 6. 发布后校验

```bash
# 校验和
sha256sum -c SHA256SUMS.txt --ignore-missing

# Linux 冒烟
sudo dpkg -i flinkops_0.1.0_amd64.deb
sudoedit /etc/flinkops/flinkops.env      # 填 FLINKOPS_CLUSTERS
sudo systemctl start flinkops && curl -fsS localhost:8080/healthz

# Windows 冒烟
# 装 setup.exe → 运行 bin\flinkops.exe → http://127.0.0.1:8080 用 admin/admin 登录

# 镜像冒烟
docker run --rm ghcr.io/state-flux/flinkops:0.1.0 /app/bin/flinkopsctl --version
```

## 7. 出问题怎么办

| 现象 | 处理 |
|---|---|
| `verify` 报版本不一致 | 按提示把 Cargo.toml / Chart.yaml / package.json 对齐后重打 tag |
| `dist` 某个平台失败 | `fail-fast: false`，其余平台照常；修好后重跑该 job，或 `gh release upload --clobber` 补一个 |
| `release` 报缺产物 | 先跑通对应平台的 `dist` job；缺平台时宁可失败也不要发半个版本 |
| 想重发同一个 tag | 删掉 Release 与 tag 后重推：`gh release delete v0.1.0 -y --cleanup-tag` |
| Windows 安装包未签名告警 | 属预期：未做代码签名，用 `SHA256SUMS.txt` 校验；如需消除需自备代码签名证书 |
| macOS 提示「已损坏」 | 未公证：`xattr -dr com.apple.quarantine flinkops-0.1.0-aarch64-apple-darwin.tar.gz` |

## 8. 当前不覆盖的部分

- `.rpm`、`AppImage`、macOS `.dmg` 与公证、Windows 代码签名：暂无，需要时在 `dist.py` 加一种 `--formats` 分支即可。
- 自动发布到 Homebrew / Scoop / winget 清单：手动提 PR 更稳，暂不做自动化。
