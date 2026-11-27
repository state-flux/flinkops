#!/usr/bin/env python3
"""FlinkOps 发行产物打包器（纯标准库，Windows / Linux / macOS 均可运行）。

用法::

    python scripts/dist.py --version 0.1.0 --target x86_64-pc-windows-msvc
    python scripts/dist.py --version 0.1.0 --target x86_64-unknown-linux-gnu --formats tar.gz,deb
    python scripts/dist.py --list-targets

产出（dist/）::

    flinkops-0.1.0-<target>.tar.gz         Linux / macOS 压缩包（根目录二进制 + web/ + start.sh + .env）
    flinkops-0.1.0-<target>.zip            Windows 便携压缩包（含 bin/ + web/ + install.ps1）
    flinkops_0.1.0_<arch>.deb              Debian / Ubuntu 安装包（systemd unit + /usr/lib/flinkops/web）
    flinkops-0.1.0-<target>-setup.exe      Windows 安装包（NSIS）
    flinkops-0.1.0-<target>.build-info.json 构建元数据（commit / rustc / 时间戳）
    flinkops-0.1.0-src.tar.gz              源码快照（git ls-files 清单，tar.gz）
    flinkops-0.1.0-src.zip                 源码快照（同一份清单，zip）
    SHA256SUMS.txt                         全部产物校验和

同一套解包目录结构同时用于压缩包和 .deb，保证「解压即用」与「包管理器安装」行为一致：
解压后 ``./start.sh``（或直接 ``./flinkops``）即启动，``web/`` 与 ``data/`` 都在同级。
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import platform
import re
import shlex
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import zipfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DIST = ROOT / "dist"

# 二进制名 -> crate（构建用），三个产物统一进同一个发行包
BINARIES = [
    ("flinkops", "flinkops-api"),      # 控制面 + SPA 托管
    ("flinkopsctl", "flinkops-cli"),   # CLI
    ("flinkops-mcp", "flinkops-mcp"),  # MCP server
]

# 目标三元组 -> (平台族, .deb 架构, 可执行后缀, 压缩格式)
TARGETS = {
    "x86_64-pc-windows-msvc": ("windows", None, ".exe", "zip"),
    "aarch64-pc-windows-msvc": ("windows", None, ".exe", "zip"),
    "x86_64-unknown-linux-gnu": ("linux", "amd64", "", "tar.gz"),
    "aarch64-unknown-linux-gnu": ("linux", "arm64", "", "tar.gz"),
    "x86_64-unknown-linux-musl": ("linux-static", "amd64", "", "tar.gz"),
    "aarch64-unknown-linux-musl": ("linux-static", "arm64", "", "tar.gz"),
    "x86_64-apple-darwin": ("macos", None, "", "tar.gz"),
    "aarch64-apple-darwin": ("macos", None, "", "tar.gz"),
}

SYSTEMD_UNIT = """\
[Unit]
Description=FlinkOps - Flink Kubernetes Operator 运维控制台
Documentation=https://github.com/state-flux/flinkops
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=flinkops
Group=flinkops
EnvironmentFile=-/etc/flinkops/flinkops.env
Environment=FLINKOPS_BIND=0.0.0.0:8080
Environment=FLINKOPS_WEB_DIR=/usr/lib/flinkops/web
Environment=FLINKOPS_STORE=sqlite:///var/lib/flinkops/flinkops.db?mode=rwc
Environment=RUST_LOG=info
WorkingDirectory=/var/lib/flinkops
StateDirectory=flinkops
ConfigurationDirectory=flinkops
ExecStart=/usr/bin/flinkops
Restart=on-failure
RestartSec=5
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/flinkops

[Install]
WantedBy=multi-user.target
"""

ENV_EXAMPLE = """\
# FlinkOps 运行环境（systemd: /etc/flinkops/flinkops.env）
# 修改后 systemctl restart flinkops 生效。

# 监听地址
FLINKOPS_BIND=0.0.0.0:8080

# 前端静态资源目录（.deb 安装在 /usr/lib/flinkops/web）
FLINKOPS_WEB_DIR=/usr/lib/flinkops/web

# 元数据库：sqlite / postgres / mysql（sqlx::Any）
FLINKOPS_STORE=sqlite:///var/lib/flinkops/flinkops.db?mode=rwc
# FLINKOPS_STORE=postgres://flinkops:secret@127.0.0.1:5432/flinkops

# 集群：id=incluster 或 id=/path/to/kubeconfig，多个用逗号分隔
# FLINKOPS_CLUSTERS=prod=/etc/flinkops/kubeconfig
FLINKOPS_CLUSTERS=local=incluster

# 首次启动的管理员账号（仅首次建库时生效，随后请在 UI 内改密）
FLINKOPS_ADMIN_USER=admin
FLINKOPS_ADMIN_PASSWORD=admin

RUST_LOG=info
"""

# 便携包（zip / tar.gz）里的 .env：相对路径以 exe 所在目录解析，双击即用。
PORTABLE_ENV = """\
# 双击 flinkops（Windows: flinkops.exe）即可启动，浏览器会自动打开 http://127.0.0.1:8080
# 改完重启生效；删掉本文件也能跑（走内置默认值）。

FLINKOPS_BIND=127.0.0.1:8080
FLINKOPS_WEB_DIR=./web
FLINKOPS_STORE=sqlite://./data/flinkops.db?mode=rwc
FLINKOPS_OPEN_BROWSER=1
RUST_LOG=info

# 集群：不填则依次尝试 in-cluster → ~/.kube/config；多集群用逗号分隔
# FLINKOPS_CLUSTERS=prod=C:/Users/me/.kube/config
"""

START_CMD = """\
@echo off
chcp 65001 >nul
cd /d "%~dp0"
echo.
echo   FlinkOps 启动中，浏览器会自动打开 http://127.0.0.1:8080
echo   关掉这个窗口即停止服务。
echo.
"%~dp0flinkops.exe"
echo.
echo   FlinkOps 已退出。
pause
"""

START_SH = """\
#!/bin/sh
# 解压即用：./start.sh（等价于直接 ./flinkops）
cd "$(dirname "$0")" || exit 1
exec ./flinkops
"""

INSTALL_SH = """\
#!/bin/sh
# FlinkOps 系统安装（可选；不装也能直接 ./start.sh 跑）
#   ./install.sh                 装到 /usr/local（需要写权限）
#   PREFIX=$HOME/.local ./install.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix="${PREFIX:-/usr/local}"
bindir="$prefix/bin"
libdir="$prefix/lib/flinkops"
datadir="${FLINKOPS_DATA_DIR:-$prefix/var/lib/flinkops}"

mkdir -p "$bindir" "$libdir" "$datadir"
for b in flinkops flinkopsctl flinkops-mcp; do
  install -m 0755 "$here/$b" "$bindir/$b"
done
rm -rf "$libdir/web"
cp -R "$here/web" "$libdir/web"

cat > "$libdir/flinkops.env" <<EOF
FLINKOPS_BIND=127.0.0.1:8080
FLINKOPS_WEB_DIR=$libdir/web
FLINKOPS_STORE=sqlite://$datadir/flinkops.db?mode=rwc
FLINKOPS_OPEN_BROWSER=1
RUST_LOG=info
EOF

echo "FlinkOps 已安装："
echo "  $bindir/flinkops      控制面 + Web UI"
echo "  $bindir/flinkopsctl   CLI"
echo "  $bindir/flinkops-mcp  MCP server"
echo "  Web 资源：$libdir/web   数据目录：$datadir   配置：$libdir/flinkops.env"
echo
echo "启动：$bindir/flinkops      # 配置：$libdir/flinkops.env"
"""

INSTALL_PS1 = """\
# FlinkOps 便携安装（可选；不装也能直接双击 flinkops.exe 跑）。
#   powershell -ExecutionPolicy Bypass -File .\\install.ps1
param(
  [string]$InstallDir = "$env:LOCALAPPDATA\\Programs\\FlinkOps",
  [switch]$NoPath,
  [switch]$NoShortcut
)
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

New-Item -ItemType Directory -Force -Path $InstallDir, "$InstallDir\\data" | Out-Null
Copy-Item "$here\\*.exe" $InstallDir -Force
Copy-Item "$here\\web" "$InstallDir\\web" -Recurse -Force
foreach ($f in @("LICENSE", "NOTICE", "README.md", "VERSION", "start.cmd", "uninstall.ps1", "flinkops.env.example")) {
  if (Test-Path "$here\\$f") { Copy-Item "$here\\$f" $InstallDir -Force }
}

$envFile = "$InstallDir\\.env"
if (-not (Test-Path $envFile)) {
  $dir = $InstallDir -replace '\\\\', '/'
  @"
FLINKOPS_BIND=127.0.0.1:8080
FLINKOPS_WEB_DIR=$dir/web
FLINKOPS_STORE=sqlite:///$dir/data/flinkops.db?mode=rwc
FLINKOPS_OPEN_BROWSER=1
RUST_LOG=info
"@ | Set-Content -Encoding ASCII $envFile
}

if (-not $NoPath) {
  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($userPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
    Write-Host "已把 $InstallDir 加入用户 PATH（重开终端生效）"
  }
}

if (-not $NoShortcut) {
  $shell = New-Object -ComObject WScript.Shell
  $lnk = $shell.CreateShortcut("$([Environment]::GetFolderPath('StartMenu'))\\Programs\\FlinkOps.lnk")
  $lnk.TargetPath = "$InstallDir\\flinkops.exe"
  $lnk.WorkingDirectory = $InstallDir
  $lnk.Save()
}

Write-Host "FlinkOps 已安装到 $InstallDir"
Write-Host "  启动：$InstallDir\\flinkops.exe（浏览器会自动打开 http://127.0.0.1:8080）"
Write-Host "  卸载：$InstallDir\\uninstall.ps1"
"""

UNINSTALL_PS1 = """\
param([string]$InstallDir = "$env:LOCALAPPDATA\\Programs\\FlinkOps")
$ErrorActionPreference = "Continue"
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -like "*$InstallDir*") {
  [Environment]::SetEnvironmentVariable("Path", ($userPath -split ';' | Where-Object { $_ -ne $InstallDir }) -join ';', "User")
}
Remove-Item "$([Environment]::GetFolderPath('StartMenu'))\\Programs\\FlinkOps.lnk" -ErrorAction SilentlyContinue
$data = Join-Path $InstallDir "data"
if (Test-Path $data) {
  if ((Read-Host "是否同时删除数据目录 $data ？(y/N)") -match '^[yY]') { Remove-Item $data -Recurse -Force }
}
Remove-Item $InstallDir -Recurse -Force -ErrorAction SilentlyContinue
Write-Host "FlinkOps 已卸载"
"""


# ---------------------------------------------------------------- helpers
def sh(cmd: list[str]) -> str:
    try:
        return subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT).stdout.strip()
    except FileNotFoundError:
        return ""


def read_version_from_cargo() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"^\[workspace\.package\](.*?)(^\[|\Z)", text, re.S | re.M)
    if not m:
        raise SystemExit("Cargo.toml 缺少 [workspace.package]")
    v = re.search(r'^version\s*=\s*"([^"]+)"', m.group(1), re.M)
    if not v:
        raise SystemExit("Cargo.toml 缺少 [workspace.package].version")
    return v.group(1)


def normalise_version(v: str) -> str:
    return v[1:] if v.startswith("v") else v


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


_EPOCH_SOURCE = "clock"


def epoch() -> int:
    """可复现构建：SOURCE_DATE_EPOCH 优先，否则用 git commit 时间，最后才用当前时钟。"""
    global _EPOCH_SOURCE
    env = os.environ.get("SOURCE_DATE_EPOCH")
    if env and env.isdigit():
        _EPOCH_SOURCE = "SOURCE_DATE_EPOCH"
        return int(env)
    ts = sh(["git", "log", "-1", "--format=%ct"])
    if ts.isdigit():
        _EPOCH_SOURCE = "git-commit"
        return int(ts)
    _EPOCH_SOURCE = "clock"
    return int(time.time())


def chmod_x(path: Path) -> None:
    mode = path.stat().st_mode
    path.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def build_info(target: str, version: str, args: argparse.Namespace) -> dict:
    return {
        "product": "flinkops",
        "version": version,
        "target": target,
        "commit": args.commit or sh(["git", "rev-parse", "HEAD"]),
        "commit_date": sh(["git", "log", "-1", "--format=%cI"]),
        "rustc": sh(["rustc", "--version"]),
        "cargo": sh(["cargo", "--version"]),
        "node": sh(["node", "--version"]),
        "built_at": datetime.fromtimestamp(epoch(), tz=timezone.utc).isoformat().replace("+00:00", "Z"),
        "timestamp_source": _EPOCH_SOURCE,
        "built_on": f"{sys.platform}/{os.cpu_count()}cpu",
    }


# ---------------------------------------------------------------- staging
def artifact_name(fmt: str, version: str, target: str) -> str:
    """产物文件名（唯一来源：rele
ase_notes.py 也从这里取，避免文案与产物漂移）。"""
    if fmt == "tar.gz":
        return f"flinkops-{version}-{target}.tar.gz"
    if fmt == "zip":
        return f"flinkops-{version}-{target}.zip"
    if fmt == "deb":
        return f"flinkops_{version}_{TARGETS[target][1]}.deb"
    if fmt == "setup":
        return f"flinkops-{version}-{target}-setup.exe"
    if fmt == "appimage":
        return f"flinkops-{version}-{target}.AppImage"
    if fmt == "appdir":
        return f"flinkops-{version}-{target}.AppDir.tar.gz"
    if fmt == "src-tar.gz":
        return f"flinkops-{version}-src.tar.gz"
    if fmt == "src-zip":
        return f"flinkops-{version}-src.zip"
    raise SystemExit(f"未知格式 {fmt}")


def stage(target: str, version: str, args: argparse.Namespace, workdir: Path) -> Path:
    """解包目录布局（压缩包 / 安装包共用，保证「解压即用」与「安装后」结构一致）::

        flinkops[-.exe]  flinkopsctl[-.exe]  flinkops-mcp[-.exe]
        web/  .env  start.cmd|start.sh  install.*  systemd/  LICENSE NOTICE README.md VERSION build-info.json
    """
    family, _deb_arch, exe, _ = TARGETS[target]
    root = workdir / f"flinkops-{version}-{target}"
    root.mkdir(parents=True)
    (root / "web").mkdir(parents=True)
    (root / "data").mkdir(parents=True)

    # 二进制放在**包根目录**：双击 flinkops.exe / ./flinkops 就能跑，不需要先"安装"
    for name, _crate in BINARIES:
        src = Path(args.bin_dir) / (name + exe)
        if not src.exists():
            raise SystemExit(f"缺少二进制：{src}（先 cargo build --release --target {target}）")
        dst = root / (name + exe)
        shutil.copy2(src, dst)
        if exe == "":
            chmod_x(dst)

    web = Path(args.web_dir)
    if not (web / "index.html").exists():
        raise SystemExit(f"缺少前端产物：{web / 'index.html'}（先 cd apps/web && npm run build）")
    for item in web.iterdir():
        if item.is_dir():
            shutil.copytree(item, root / "web" / item.name)
        else:
            shutil.copy2(item, root / "web" / item.name)

    for doc in ("LICENSE", "NOTICE"):
        if (ROOT / doc).exists():
            shutil.copy2(ROOT / doc, root / doc)
    readme = ROOT / "README.zh-CN.md" if family != "windows" and (ROOT / "README.zh-CN.md").exists() else ROOT / "README.md"
    if readme.exists():
        shutil.copy2(readme, root / "README.md")

    (root / "VERSION").write_text(version + "\n", encoding="utf-8")
    info = build_info(target, version, args)
    (root / "build-info.json").write_text(json.dumps(info, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    # 零配置 .env：双击启动时由程序自己加载（相对路径以 exe 所在目录解析）
    (root / ".env").write_text(PORTABLE_ENV, encoding="utf-8")

    if family == "windows":
        (root / "start.cmd").write_text(START_CMD, encoding="utf-8-sig")
        (root / "install.ps1").write_text(INSTALL_PS1, encoding="utf-8-sig")
        (root / "uninstall.ps1").write_text(UNINSTALL_PS1, encoding="utf-8-sig")
    else:
        (root / "start.sh").write_text(START_SH, encoding="utf-8")
        chmod_x(root / "start.sh")
        (root / "install.sh").write_text(INSTALL_SH, encoding="utf-8")
        chmod_x(root / "install.sh")
        (root / "systemd").mkdir()
        (root / "systemd" / "flinkops.service").write_text(SYSTEMD_UNIT, encoding="utf-8")
    return root


# ---------------------------------------------------------------- archives
def _archive_mode(rel: str) -> int:
    """归档内权限：bin/ 下与 *.sh 可执行，其余 0644。

    不能用 os.stat 判断——Windows 上 st_mode 不带执行位，会让打包器在 Windows 主机上
    生成 0644 的二进制，解包后不可执行。
    """
    p = Path(rel)
    if p.parent.name == "bin" or p.name.endswith((".sh", ".command")):
        return 0o755
    # 包根目录下的裸二进制（无扩展名）：flinkops / flinkopsctl / flinkops-mcp
    if p.parent == Path(".") and p.name in {name for name, _ in BINARIES}:
        return 0o755
    return 0o644


def _tar_add(tar: tarfile.TarFile, src: Path, arcname: str, rel: str, mtime: int) -> None:
    info = tar.gettarinfo(str(src), arcname)
    info.mtime = mtime
    info.uid = info.gid = 0
    info.uname = info.gname = "root"
    info.mode = 0o755 if src.is_dir() else _archive_mode(rel)
    if src.is_file():
        with src.open("rb") as fh:
            tar.addfile(info, fh)
    else:
        tar.addfile(info)
        for child in sorted(src.iterdir()):
            child_rel = f"{rel}/{child.name}" if rel else child.name
            _tar_add(tar, child, f"{arcname}/{child.name}", child_rel, mtime)


def write_tar_gz(root: Path, out: Path, mtime: int) -> None:
    with out.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=mtime, compresslevel=9) as gz:
        with tarfile.open(fileobj=gz, mode="w", format=tarfile.GNU_FORMAT) as tar:
            _tar_add(tar, root, root.name, "", mtime)


def write_zip(root: Path, out: Path, mtime: int) -> None:
    stamp = time.gmtime(max(mtime, 315532800))[:6]
    with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
        for path in sorted(root.rglob("*")):
            if path.is_dir():
                continue
            rel = path.relative_to(root).as_posix()
            info = zipfile.ZipInfo(f"{root.name}/{rel}", date_time=stamp)
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = _archive_mode(rel) << 16
            zf.writestr(info, path.read_bytes())


# ---------------------------------------------------------------- deb
AR_MAGIC = b"!<arch>\n"


def _ar_member(name: str, data: bytes, mtime: int) -> bytes:
    header = f"{name:<16}{mtime:<12}{0:<6}{0:<6}{100644:<8}{len(data):<10}`\n".encode()
    assert len(header) == 60, header
    pad = b"\n" if len(data) % 2 else b""
    return header + data + pad


def _tar_bytes(entries: list[tuple[str, bytes | None, int]], mtime: int) -> bytes:
    """entries: (arcname, content 或 None 表示目录, mode)。"""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode="w", format=tarfile.GNU_FORMAT) as tar:
        for arcname, content, mode in entries:
            info = tarfile.TarInfo(arcname)
            info.mtime = mtime
            info.uid = info.gid = 0
            info.uname = info.gname = "root"
            info.mode = mode
            if content is None:
                info.type = tarfile.DIRTYPE
                tar.addfile(info)
            else:
                info.size = len(content)
                tar.addfile(info, io.BytesIO(content))
    return gzip.compress(buf.getvalue(), compresslevel=9, mtime=mtime)


DEB_POSTINST = """\
#!/bin/sh
set -e
if ! getent group flinkops >/dev/null; then
  addgroup --system flinkops >/dev/null
fi
if ! getent passwd flinkops >/dev/null; then
  adduser --system --ingroup flinkops --home /var/lib/flinkops --no-create-home \\
    --gecos "FlinkOps service account" flinkops >/dev/null
fi
install -d -o flinkops -g flinkops -m 0750 /var/lib/flinkops
install -d -m 0750 /etc/flinkops
if [ ! -f /etc/flinkops/flinkops.env ]; then
  cp /usr/share/doc/flinkops/flinkops.env.example /etc/flinkops/flinkops.env
fi
chmod 0640 /etc/flinkops/flinkops.env
chown root:flinkops /etc/flinkops/flinkops.env 2>/dev/null || true
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi
echo "FlinkOps 安装完成：编辑 /etc/flinkops/flinkops.env ，然后 systemctl enable --now flinkops"
exit 0
"""

DEB_PRERM = """\
#!/bin/sh
set -e
if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
  systemctl stop flinkops.service >/dev/null 2>&1 || true
fi
exit 0
"""

DEB_POSTRM = """\
#!/bin/sh
set -e
if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi
if [ "$1" = "purge" ]; then
  rm -rf /var/lib/flinkops /etc/flinkops
  if getent passwd flinkops >/dev/null; then deluser --system flinkops >/dev/null 2>&1 || true; fi
  if getent group flinkops >/dev/null; then delgroup --system flinkops >/dev/null 2>&1 || true; fi
fi
exit 0
"""


def write_deb(root: Path, out: Path, version: str, target: str, mtime: int) -> None:
    _, arch, exe, _ = TARGETS[target]
    if arch is None:
        raise SystemExit(f"{target} 不是 Linux 目标，无法生成 .deb")
    if exe:
        raise SystemExit(".deb 需要无扩展名的 Linux 二进制")

    data: list[tuple[str, bytes | None, int]] = [("./", None, 0o755)]
    for name, _crate in BINARIES:
        data.append((f"./usr/bin/{name}", (root / name).read_bytes(), 0o755))
    for path in sorted((root / "web").rglob("*")):
        rel = f"./usr/lib/flinkops/web/{path.relative_to(root / 'web').as_posix()}"
        data.append((rel, None, 0o755) if path.is_dir() else (rel, path.read_bytes(), 0o644))
    data.append(("./usr/lib/flinkops/web", None, 0o755))
    data.append(("./lib/systemd/system/flinkops.service", SYSTEMD_UNIT.encode(), 0o644))
    data.append(("./etc/flinkops/", None, 0o750))
    data.append(("./etc/flinkops/flinkops.env", ENV_EXAMPLE.encode(), 0o640))
    data.append(("./usr/share/doc/flinkops/README.md", (root / "README.md").read_bytes() if (root / "README.md").exists() else b"", 0o644))
    data.append(("./usr/share/doc/flinkops/flinkops.env.example", ENV_EXAMPLE.encode(), 0o644))
    data.append(("./usr/share/doc/flinkops/copyright", (root / "LICENSE").read_bytes() if (root / "LICENSE").exists() else b"", 0o644))
    data.append(("./usr/share/doc/flinkops/build-info.json", (root / "build-info.json").read_bytes(), 0o644))
    if (root / "NOTICE").exists():
        data.append(("./usr/share/doc/flinkops/NOTICE", (root / "NOTICE").read_bytes(), 0o644))

    md5_lines = []
    for arcname, content, _mode in data:
        if content is not None:
            md5_lines.append(f"{hashlib.md5(content).hexdigest()}  {arcname[2:]}")
    md5sums = ("\n".join(sorted(md5_lines)) + "\n").encode()

    installed_size = sum(len(c) for _a, c, _m in data if c is not None) // 1024 + 1
    control = f"""\
Package: flinkops
Version: {version}
Section: admin
Priority: optional
Architecture: {arch}
Maintainer: state-flux <maintainers@state-flux.dev>
Installed-Size: {installed_size}
Depends: libc6 (>= 2.34), ca-certificates
Recommends: kubectl
Homepage: https://github.com/state-flux/flinkops
Description: Flink Kubernetes Operator 运维控制台
 FlinkOps 是面向 Apache Flink on Kubernetes 的运维控制台：
 作业生命周期（start/suspend/restart/savepoint/restore/scale/upgrade）、
 只读 Flink Web UI 反向代理、Prometheus 指标穿透、告警规则与审计日志。
 单个 Rust 二进制同时托管 Vue 3 前端，元数据落 SQLite / PostgreSQL / MySQL。
 本包含 flinkops（控制面）、flinkopsctl（CLI）、flinkops-mcp（MCP server）。
""".encode()

    control_entries = [
        ("./", None, 0o755),
        ("./control", control, 0o644),
        ("./conffiles", b"/etc/flinkops/flinkops.env\n", 0o644),
        ("./md5sums", md5sums, 0o644),
        ("./postinst", DEB_POSTINST.encode(), 0o755),
        ("./prerm", DEB_PRERM.encode(), 0o755),
        ("./postrm", DEB_POSTRM.encode(), 0o755),
    ]
    # /etc/flinkops/flinkops.env 随包分发并登记为 conffile：升级时保留用户改动
    data.append(("./etc/flinkops/flinkops.env", ENV_EXAMPLE.encode(), 0o640))

    blob = (
        AR_MAGIC
        + _ar_member("debian-binary", b"2.0\n", mtime)
        + _ar_member("control.tar.gz", _tar_bytes(control_entries, mtime), mtime)
        + _ar_member("data.tar.gz", _tar_bytes(data, mtime), mtime)
    )
    out.write_bytes(blob)


# ---------------------------------------------------------------- nsis setup
def find_makensis() -> str | None:
    """找 NSIS 的 makensis：环境变量 > 仓库内 .tools/ > PATH。"""
    env = os.environ.get("MAKENSIS")
    if env and Path(env).exists():
        return env
    for cand in (
        ROOT / ".tools" / "nsis" / "makensis.exe",
        ROOT / ".tools" / "nsis" / "makensis",
        Path.home() / ".tools" / "nsis" / "makensis.exe",
    ):
        if cand.exists():
            return str(cand)
    return shutil.which("makensis")


def write_setup(root: Path, out: Path, version: str, target: str) -> None:
    if sys.platform != "win32" and not shutil.which("makensis"):
        raise SystemExit("NSIS 安装包只能在装有 makensis 的机器上生成（Windows，或 Linux 上的 nsis 包）")
    makensis = find_makensis()
    if not makensis:
        raise SystemExit(
            "未找到 makensis。设置 MAKENSIS=<路径>，或把 NSIS 解压到 .tools/nsis/，"
            "或 choco install nsis / apt-get install nsis"
        )
    v4 = ".".join((version.split("-")[0].split(".") + ["0", "0", "0"])[:4])
    script = ROOT / "scripts" / "windows" / "flinkops.nsi"
    # NSIS 把 File/OutFile 的相对路径解析到**脚本所在目录**，所以把脚本副本放进 stage 根目录，
    # 用 /DSTAGE=. 定位；OutFile 仍然给绝对路径。这样与路径里有没有空格无关。
    script_copy = root / "_flinkops-setup.nsi"
    shutil.copy2(script, script_copy)
    cmd = [
        makensis,
        "/V2",
        f"/DVERSION={version}",
        f"/DVERSION4={v4}",
        f"/DTARGET={target}",
        "/DSTAGE=.",
        f'/DOUTFILE="{out.resolve()}"',
        str(script_copy),
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    stray = script.parent / out.name
    if not out.exists() and stray.exists():  # 旧版 NSIS 的路径解析差异兜底
        shutil.move(str(stray), str(out))
    if proc.returncode != 0 or not out.exists():
        sys.stdout.write(proc.stdout)
        sys.stderr.write(proc.stderr)
        raise SystemExit(f"makensis 失败（{proc.returncode}）")
    if "warning" in proc.stdout.lower():
        for line in proc.stdout.splitlines():
            if "warning" in line.lower():
                print(f"[nsis] {line.strip()}")


# ---------------------------------------------------------------- source archives
SRC_FORMATS = ("src-tar.gz", "src-zip")


def git_sources() -> list[tuple[str, int]]:
    """源码快照清单：(相对路径, 权限)。包含已跟踪文件 + 未忽略的新文件，跳过 .gitignore 命中的内容。"""
    try:
        raw = subprocess.run(
            ["git", "ls-files", "-s", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=ROOT,
            capture_output=True,
        ).stdout
    except FileNotFoundError:
        raise SystemExit("生成源码包需要 git")
    entries: list[tuple[str, int]] = []
    seen: set[str] = set()
    # 保险：即便某个构建目录没写进 .gitignore，也不进源码包
    blocked = {"dist", "release", "target", "target-fix", "target-verify", "node_modules", "data", ".tools"}
    for record in raw.split(b"\0"):
        if not record.strip():
            continue
        # 已跟踪文件是 "<mode> <sha> <stage>\t<path>"，未跟踪文件只有 "<path>"
        if b"\t" in record:
            meta, _, path = record.partition(b"\t")
            try:
                mode = int(meta.split(b" ")[0], 8)
            except ValueError:
                mode = 0o644
        else:
            path, mode = record, 0o644
        if path in seen:  # --cached 与 --others 去重
            continue
        decoded = path.decode("utf-8", "surrogateescape")
        head = decoded.split("/", 1)[0]
        if head in blocked or "/node_modules/" in decoded:
            continue
        seen.add(path)
        entries.append((path.decode("utf-8", "surrogateescape"), 0o755 if mode & 0o111 else 0o644))
    return sorted(entries)


def write_source_archives(version: str, formats: list[str], out_dir: Path, mtime: int) -> list[Path]:
    files = git_sources()
    if not files:
        raise SystemExit("源码清单为空：git ls-files 没有返回任何文件")
    root_name = f"flinkops-{version}-src"
    produced: list[Path] = []

    for fmt in formats:
        if fmt == "src-tar.gz":
            out = out_dir / artifact_name("src-tar.gz", version, "src")
            with out.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=mtime, compresslevel=9) as gz:
                with tarfile.open(fileobj=gz, mode="w", format=tarfile.GNU_FORMAT) as tar:
                    dirs: set[str] = set()
                    for rel, mode in files:
                        for parent in Path(rel).parents:
                            arc = f"{root_name}/{parent.as_posix()}"
                            if str(parent) != "." and arc not in dirs:
                                dirs.add(arc)
                                info = tarfile.TarInfo(arc)
                                info.mtime = mtime
                                info.mode = 0o755
                                info.uid = info.gid = 0
                                info.uname = info.gname = "root"
                                info.type = tarfile.DIRTYPE
                                tar.addfile(info)
                        info = tarfile.TarInfo(f"{root_name}/{rel}")
                        info.mtime = mtime
                        info.mode = mode
                        info.uid = info.gid = 0
                        info.uname = info.gname = "root"
                        info.size = (ROOT / rel).stat().st_size
                        with (ROOT / rel).open("rb") as fh:
                            tar.addfile(info, fh)
        elif fmt == "src-zip":
            out = out_dir / artifact_name("src-zip", version, "src")
            stamp = time.gmtime(max(mtime, 315532800))[:6]
            with zipfile.ZipFile(out, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
                for rel, mode in files:
                    info = zipfile.ZipInfo(f"{root_name}/{rel}", date_time=stamp)
                    info.compress_type = zipfile.ZIP_DEFLATED
                    info.external_attr = mode << 16
                    zf.writestr(info, (ROOT / rel).read_bytes())
        else:  # pragma: no cover - 调用方已过滤
            continue
        produced.append(out)
        print(f"[dist] {out.relative_to(ROOT) if out.is_relative_to(ROOT) else out}  ({out.stat().st_size / 1024 / 1024:.1f} MiB)")
    return produced


# ---------------------------------------------------------------- AppImage
DESKTOP_FILE = """\
[Desktop Entry]
Type=Application
Name=FlinkOps
GenericName=Flink Apache Flink Kubernetes Operator 控制台
Comment=作业生命周期、只读 Flink UI、指标、告警与审计
Exec=flinkops
Icon=flinkops
Terminal=true
Categories=Development;System;Monitor;
Keywords=Flink;Kubernetes;Operator;Metrics;
StartupNotify=false
"""

APPRUN_SCRIPT = """\
#!/bin/sh
# AppImage 入口：把 web / 数据 / 浏览器都指到 AppImage 自己能用的位置
HERE=$(dirname "$(readlink -f "$0")")
export FLINKOPS_WEB_DIR="$HERE/usr/share/flinkops/web"
export FLINKOPS_STORE="${FLINKOPS_STORE:-sqlite://$HOME/.local/share/flinkops/flinkops.db?mode=rwc}"
export FLINKOPS_BIND="${FLINKOPS_BIND:-127.0.0.1:8080}"
if [ -z "${FLINKOPS_OPEN_BROWSER+x}" ]; then export FLINKOPS_OPEN_BROWSER=1; fi
mkdir -p "$HOME/.local/share/flinkops"
exec "$HERE/usr/bin/flinkops" "$@"
"""


def _png_icon(size: int = 256) -> bytes:
    """画一个纯标准库生成的应用图标（橙色底 + 白色 F），避免引入 Pillow。"""
    import struct
    import zlib

    bg = (232, 116, 42)      # Flink 橙
    fg = (255, 255, 255)
    px = [[bg for _ in range(size)] for _ in range(size)]

    def rect(x0, y0, x1, y1):
        for y in range(max(0, y0), min(size, y1)):
            row = px[y]
            for x in range(max(0, x0), min(size, x1)):
                row[x] = fg

    u = size // 8                       # 栅格单位
    # 字母 F：竖笔 + 上横 + 中横
    rect(3 * u, 2 * u, 4 * u, 6 * u)
    rect(3 * u, 2 * u, 6 * u, 3 * u)
    rect(3 * u, 4 * u, 5 * u, 5 * u)

    raw = b"".join(b"\x00" + bytes(v for pixel in row for v in pixel) for row in px)

    def chunk(tag: bytes, data: bytes) -> bytes:
        return (
            struct.pack(">I", len(data))
            + tag
            + data
            + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)
        )

    header = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)  # 8-bit RGB
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )


def _appimage_arch(target: str) -> str:
    return "aarch64" if target.startswith("aarch64") else "x86_64"


def build_appdir(root: Path, appdir: Path, version: str) -> Path:
    """把发行目录摊成 AppDir：usr/bin + usr/share/flinkops/web + AppRun + .desktop + 图标。"""
    if appdir.exists():
        shutil.rmtree(appdir)
    (appdir / "usr" / "bin").mkdir(parents=True)
    (appdir / "usr" / "share" / "flinkops").mkdir(parents=True)
    (appdir / "usr" / "share" / "applications").mkdir(parents=True)
    (appdir / "usr" / "share" / "icons" / "hicolor" / "256x256" / "apps").mkdir(parents=True)

    for name, _crate in BINARIES:
        src = root / name
        if not src.exists():  # windows 目录里是 .exe
            src = root / (name + ".exe")
        if not src.exists():
            raise SystemExit(f"AppDir 缺少二进制：{name}")
        dst = appdir / "usr" / "bin" / name
        shutil.copy2(src, dst)
        chmod_x(dst)

    shutil.copytree(root / "web", appdir / "usr" / "share" / "flinkops" / "web")

    icon = _png_icon()
    (appdir / "flinkops.png").write_bytes(icon)
    (appdir / "usr" / "share" / "icons" / "hicolor" / "256x256" / "apps" / "flinkops.png").write_bytes(icon)
    (appdir / "flinkops.desktop").write_text(DESKTOP_FILE, encoding="utf-8")
    (appdir / "usr" / "share" / "applications" / "flinkops.desktop").write_text(DESKTOP_FILE, encoding="utf-8")

    apprun = appdir / "AppRun"
    apprun.write_text(APPRUN_SCRIPT, encoding="utf-8")
    chmod_x(apprun)
    (appdir / "VERSION").write_text(version + "\n", encoding="utf-8")
    return appdir


def find_mksquashfs() -> str | None:
    env = os.environ.get("MKSQUASHFS")
    if env and Path(env).exists():
        return env
    for cand in (ROOT / ".tools" / "appimage" / "mksquashfs.exe", ROOT / ".tools" / "appimage" / "mksquashfs"):
        if cand.exists():
            return str(cand)
    return shutil.which("mksquashfs")


def find_appimage_runtime(arch: str) -> Path | None:
    env = os.environ.get("APPIMAGE_RUNTIME")
    if env and Path(env).exists():
        return Path(env)
    base = ROOT / ".tools" / "appimage"
    for name in (f"runtime-{arch}", f"runtime-{arch}.AppImage", "runtime"):
        cand = base / name
        if cand.exists():
            return cand
    return None


def _squash_cmd(tool: str, *args: str) -> list[str]:
    """mksquashfs 可能是原生可执行文件，也可能是 .py/.cmd 包装脚本。"""
    suffix = Path(tool).suffix.lower()
    if suffix == ".py":
        return [sys.executable, tool, *args]
    if suffix in (".cmd", ".bat"):
        return [os.environ.get("COMSPEC", "cmd.exe"), "/c", tool, *args]
    return [tool, *args]


def write_appimage(root: Path, out: Path, version: str, target: str, workdir: Path) -> None:
    """AppImage = 官方 runtime（ELF 头） + squashfs 文件系统。需要 mksquashfs 与 runtime。"""
    arch = _appimage_arch(target)
    mksquashfs = find_mksquashfs()
    runtime = find_appimage_runtime(arch)
    if not mksquashfs:
        raise SystemExit(
            "生成 AppImage 需要 mksquashfs：Debian/Ubuntu `apt-get install squashfs-tools`；"
            "或设置 MKSQUASHFS=<路径>；没有的话改用 --formats tar.gz,deb"
        )
    if not runtime:
        raise SystemExit(
            f"生成 AppImage 需要官方 runtime（{arch}）：\n"
            f"  mkdir -p .tools/appimage && curl -fsSL -o .tools/appimage/runtime-{arch} \\\n"
            f"    https://github.com/AppImage/AppImageKit/releases/download/continuous/runtime-{arch}\n"
            f"  chmod +x .tools/appimage/runtime-{arch}     # 或设置 APPIMAGE_RUNTIME=<路径>"
        )

    appdir = build_appdir(root, workdir / f"flinkops-{version}.AppDir", version)
    squash = workdir / "flinkops.squashfs"
    for comp in ("zstd", "gzip", "xz"):
        cmd = _squash_cmd(mksquashfs, str(appdir), str(squash),
                          "-root-owned", "-noappend", "-no-xattrs", "-quiet", "-comp", comp)
        proc = subprocess.run(cmd, capture_output=True, text=True, errors="replace")
        if proc.returncode == 0:
            break
    else:
        sys.stderr.write(proc.stdout + proc.stderr)
        raise SystemExit("mksquashfs 失败（试过 zstd / gzip / xz）")

    out.write_bytes(runtime.read_bytes() + squash.read_bytes())
    chmod_x(out)


def write_sha256_sidecar(path: Path) -> Path:
    """`<file>.sha256`：与 `sha256sum` 输出同格式，`sha256sum -c file.tar.gz.sha256` 可直接用。"""
    sidecar = path.with_name(path.name + ".sha256")
    sidecar.write_text(f"{sha256_file(path)}  {path.name}\n", encoding="utf-8")
    return sidecar


# ---------------------------------------------------------------- main
def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="FlinkOps 发行产物打包器")
    ap.add_argument("--version", help="版本号（默认取 Cargo.toml workspace.package.version）")
    ap.add_argument("--target", help="目标三元组，例如 x86_64-unknown-linux-gnu")
    ap.add_argument("--bin-dir", help="release 二进制目录（默认 target/<target>/release）")
    ap.add_argument("--web-dir", default=str(ROOT / "apps" / "web" / "dist"), help="前端产物目录")
    ap.add_argument("--out", default=str(DIST), help="输出目录（默认 dist/）")
    ap.add_argument("--formats", help="逗号分隔：tar.gz,zip,deb,appimage,appdir,setup,src-tar.gz,src-zip（默认按目标平台选择）")
    ap.add_argument("--commit", help="覆盖 build-info 里的 commit")
    ap.add_argument("--list-targets", action="store_true", help="列出支持的目标")
    ap.add_argument("--list-formats", action="store_true", help="列出支持的产物格式")
    args = ap.parse_args(argv)

    if args.list_formats:
        rows = [
            ("tar.gz", "全部 Unix 目标", "flinkops-<ver>-<target>.tar.gz", "二进制 + web + start.sh + .env"),
            ("zip", "Windows", "flinkops-<ver>-<target>.zip", "二进制 + web + install.ps1"),
            ("deb", "Linux (glibc)", "flinkops_<ver>_<arch>.deb", "systemd unit + /usr/lib/flinkops"),
            ("appimage", "Linux", "flinkops-<ver>-<target>.AppImage", "AppImage（需 mksquashfs + runtime）"),
            ("appdir", "Linux", "flinkops-<ver>-<target>.AppDir.tar.gz", "AppDir 目录树（AppImage 调试/降级）"),
            ("setup", "Windows", "flinkops-<ver>-<target>-setup.exe", "NSIS 安装包"),
            ("src-tar.gz", "任意", "flinkops-<ver>-src.tar.gz", "源码快照（git ls-files）"),
            ("src-zip", "任意", "flinkops-<ver>-src.zip", "源码快照（git ls-files）"),
        ]
        for fmt, platform_, name, note in rows:
            print(f"{fmt:<12} {platform_:<16} {name:<38} {note}")
        return 0

    if args.list_targets:
        for t, (fam, arch, _exe, fmt) in TARGETS.items():
            print(f"{t:<28} {fam:<12} deb={arch or '-':<6} 默认格式={fmt}")
        return 0

    version = normalise_version(args.version or read_version_from_cargo())
    target = args.target or default_target()
    if target not in TARGETS:
        raise SystemExit(f"不支持的目标 {target}，可用：{', '.join(TARGETS)}")

    family, _deb_arch, exe, default_fmt = TARGETS[target]
    args.bin_dir = args.bin_dir or str(ROOT / "target" / target / "release")
    if not Path(args.bin_dir).exists():
        fallback = ROOT / "target" / "release"
        if fallback.exists():
            args.bin_dir = str(fallback)
    out_dir = Path(args.out)
    out_dir.mkdir(parents=True, exist_ok=True)

    formats = [f.strip() for f in (args.formats.split(",") if args.formats else [default_fmt])]
    if "deb" in formats and family != "linux":
        raise SystemExit(f"{target} 无法生成 .deb（.deb 只针对 glibc 目标，musl 目标发 tar.gz）")
    if "setup" in formats and family != "windows":
        raise SystemExit(f"{target} 无法生成 Windows 安装包")
    if any(f in ("appimage", "appdir") for f in formats) and not family.startswith("linux"):
        raise SystemExit(f"{target} 无法生成 AppImage（AppImage 只针对 Linux 目标）")

    mtime = epoch()
    produced: list[Path] = []
    binary_formats = [f for f in formats if f not in SRC_FORMATS]

    if binary_formats:
        with tempfile.TemporaryDirectory(prefix="flinkops-dist-") as tmp:
            root = stage(target, version, args, Path(tmp))
            (root / "build-info.json").write_text(
                json.dumps(build_info(target, version, args), indent=2, ensure_ascii=False) + "\n",
                encoding="utf-8",
            )
            for fmt in binary_formats:
                if fmt == "tar.gz":
                    p = out_dir / artifact_name(fmt, version, target)
                    write_tar_gz(root, p, mtime)
                elif fmt == "zip":
                    p = out_dir / artifact_name(fmt, version, target)
                    write_zip(root, p, mtime)
                elif fmt == "deb":
                    p = out_dir / artifact_name(fmt, version, target)
                    write_deb(root, p, version, target, mtime)
                elif fmt == "setup":
                    p = out_dir / artifact_name(fmt, version, target)
                    write_setup(root, p, version, target)
                elif fmt == "appimage":
                    p = out_dir / artifact_name(fmt, version, target)
                    write_appimage(root, p, version, target, Path(tmp))
                elif fmt == "appdir":
                    appdir = build_appdir(root, Path(tmp) / f"flinkops-{version}-{target}.AppDir", version)
                    p = out_dir / artifact_name(fmt, version, target)
                    write_tar_gz(appdir, p, mtime)
                else:
                    raise SystemExit(f"未知格式 {fmt}")
                produced.append(p)
                if fmt in ("tar.gz", "zip", "deb", "setup", "appimage", "appdir"):
                    produced.append(write_sha256_sidecar(p))
                print(f"[dist] {p.relative_to(ROOT) if p.is_relative_to(ROOT) else p}  ({p.stat().st_size / 1024 / 1024:.1f} MiB)")

            info_path = out_dir / f"flinkops-{version}-{target}.build-info.json"
            shutil.copy2(root / "build-info.json", info_path)
            produced.append(info_path)

    src_formats = [f for f in formats if f in SRC_FORMATS]
    if src_formats:
        for p in write_source_archives(version, src_formats, out_dir, mtime):
            produced.append(p)
            produced.append(write_sha256_sidecar(p))

    # SHA256SUMS.txt：聚合 dist/ 下所有发行产物（增量追加，便于多平台产物汇总）
    sums = out_dir / "SHA256SUMS.txt"
    existing: dict[str, str] = {}
    if sums.exists():
        for line in sums.read_text(encoding="utf-8").splitlines():
            parts = line.split(None, 1)
            if len(parts) == 2:
                existing[parts[1].strip().lstrip("*")] = parts[0]
    for p in produced:
        if p.name.endswith(".sha256"):
            continue
        existing[p.name] = sha256_file(p)
    sums.write_text(
        "".join(f"{digest}  {name}\n" for name, digest in sorted(existing.items())), encoding="utf-8"
    )
    print(f"[dist] {sums.relative_to(ROOT) if sums.is_relative_to(ROOT) else sums}")
    return 0


def default_target() -> str:
    """按当前主机推断目标三元组（本机构建时不用手写）。"""
    machine = (platform.machine() or os.environ.get("PROCESSOR_ARCHITECTURE", "")).lower()
    arch = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
    if sys.platform == "win32":
        return f"{arch}-pc-windows-msvc"
    if sys.platform == "darwin":
        return f"{arch}-apple-darwin"
    return f"{arch}-unknown-linux-gnu"


if __name__ == "__main__":
    sys.exit(main())
