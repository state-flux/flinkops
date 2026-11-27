#!/usr/bin/env python3
"""生成 GitHub Release 正文（release notes）。

产物清单**不是手写的**：直接从 `scripts/dist.py` 导入目标矩阵与 `artifact_name()`，
因此文案永远和打包器实际产出的文件名一致（改名只需改 dist.py 一处）。

用法::

    python scripts/release_notes.py --version 0.1.0                     # 只列 dist/ 里真实存在的产物
    python scripts/release_notes.py --version 0.1.0 --full-matrix        # 补齐 CI 会产出的整矩阵（本地预览）
    python scripts/release_notes.py --version 0.1.0 --print              # 打到 stdout
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _load_packager():
    """按路径导入 scripts/dist.py（脚本有 __main__ 守卫，可安全导入）。"""
    spec = importlib.util.spec_from_file_location("flinkops_dist", ROOT / "scripts" / "dist.py")
    if spec is None or spec.loader is None:  # pragma: no cover
        raise SystemExit("无法导入 scripts/dist.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


DIST = _load_packager()

# 展示顺序 + 中文标签 + 安装方式（kind 决定「安装」章节里的命令）
# (target triple 或 "src"/"chart", format, 标签, 安装方式)
MATRIX = [
    ("x86_64-unknown-linux-gnu", "tar.gz", "Linux x86_64 压缩包（glibc ≥ 2.34）", "tar"),
    ("x86_64-unknown-linux-gnu", "deb", "Debian / Ubuntu amd64 安装包", "deb"),
    ("x86_64-unknown-linux-gnu", "appimage", "Linux x86_64 AppImage（单文件免安装）", "appimage"),
    ("aarch64-unknown-linux-gnu", "tar.gz", "Linux arm64 压缩包（glibc ≥ 2.34）", "tar"),
    ("aarch64-unknown-linux-gnu", "deb", "Debian / Ubuntu arm64 安装包", "deb"),
    ("aarch64-unknown-linux-gnu", "appimage", "Linux arm64 AppImage（单文件免安装）", "appimage"),
    ("x86_64-apple-darwin", "tar.gz", "macOS Intel 压缩包", "tar"),
    ("aarch64-apple-darwin", "tar.gz", "macOS Apple Silicon 压缩包", "tar"),
    ("x86_64-pc-windows-msvc", "zip", "Windows x86_64 便携版", "zip"),
    ("x86_64-pc-windows-msvc", "setup", "Windows x86_64 安装包", "exe"),
    ("src", "src-tar.gz", "源码快照 tar.gz", "src"),
    ("src", "src-zip", "源码快照 zip", "src"),
    ("chart", "chart", "Helm chart", "chart"),
]


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def human(size: int) -> str:
    value = float(size)
    for unit in ("B", "KiB", "MiB", "GiB"):
        if value < 1024 or unit == "GiB":
            return f"{value:.0f} {unit}" if unit == "B" else f"{value:.1f} {unit}"
        value /= 1024
    return f"{value:.1f} GiB"


def packager_name(target: str, fmt: str, version: str) -> str:
    """交给打包器算名字：保证与 dist.py 产出的文件逐字一致。"""
    if target == "chart":
        return f"flinkops-{version}.tgz"
    if target == "src":
        return DIST.artifact_name(fmt, version, "src")
    return DIST.artifact_name(fmt, version, target)


def packager_can_build(target: str) -> bool:
    """该目标在本仓库是否有打包能力（windows/linux/macos 都在 TARGETS 里）。"""
    return target in ("src", "chart") or target in DIST.TARGETS


def planned(version: str) -> list[dict]:
    rows = []
    for target, fmt, label, kind in MATRIX:
        if not packager_can_build(target):
            continue
        rows.append(
            {"file": packager_name(target, fmt, version), "platform": label, "kind": kind, "target": target}
        )
    return rows


def collect(dist_dir: Path, version: str) -> list[dict]:
    """只认打包器规则里存在的产物 + 每个产物旁边的 .sha256 / build-info。"""
    rows = []
    for row in planned(version):
        path = dist_dir / row["file"]
        if not path.exists():
            continue
        rows.append({**row, "size": human(path.stat().st_size), "sha256": sha256(path)})
    return rows


def fill_missing(rows: list[dict], version: str) -> list[dict]:
    have = {r["file"] for r in rows}
    for row in planned(version):
        if row["file"] not in have:
            rows.append({**row, "size": "—", "sha256": ""})
    order = {r["file"]: i for i, r in enumerate(planned(version))}
    rows.sort(key=lambda r: order.get(r["file"], 999))
    return rows


def extra_files(dist_dir: Path, version: str) -> dict[str, list[str]]:
    """发行页上除产物包之外的附属文件：.sha256 sidecar、build-info、汇总校验和。"""
    sidecars, build_info = [], []
    for path in sorted(dist_dir.iterdir()):
        if not path.is_file():
            continue
        if path.name.endswith(".sha256"):
            sidecars.append(path.name)
        elif path.name.endswith(".build-info.json") and version in path.name:
            build_info.append(path.name)
    return {"sidecars": sidecars, "build_info": build_info}


def changelog_section(version: str) -> str:
    path = ROOT / "CHANGELOG.md"
    if not path.exists():
        return ""
    text = path.read_text(encoding="utf-8")
    pattern = re.compile(rf"^##\s*\[?{re.escape(version)}\]?.*?$(.*?)(?=^##\s|\Z)", re.M | re.S)
    m = pattern.search(text)
    if not m:
        return ""
    body = m.group(1)
    body = re.sub(r"^\[[^\]]+\]:\s*\S+\s*$", "", body, flags=re.M).strip()
    return body


def render(version: str, dist_dir: Path, rows: list[dict], tag: str, repo: str, full: bool) -> str:
    v = version
    base = f"https://github.com/{repo}/releases/download/{tag}"
    kinds = {r["kind"] for r in rows}
    extras = extra_files(dist_dir, v)
    out: list[str] = []
    add = out.append

    add(f"# FlinkOps {tag}")
    add("")
    add(
        "面向 **Apache Flink on Kubernetes** 的运维控制台：单个 Rust 二进制同时托管 Vue 3 前端，"
        "把 `FlinkDeployment` / `FlinkSessionJob` / `FlinkStateSnapshot` 的生命周期、只读 Flink Web UI、"
        "Prometheus 指标、告警与审计收在一个可审计的界面里。"
    )
    add("")

    section = changelog_section(v)
    add("## 本版变更")
    add("")
    if section:
        add(section)
    else:
        add(f"见 [`CHANGELOG.md`](https://github.com/{repo}/blob/{tag}/CHANGELOG.md)。")
    add("")

    add("## 下载")
    add("")
    add("| 平台 | 文件 | 大小 | SHA-256 |")
    add("|---|---|---|---|")
    for row in rows:
        digest = f"`{row['sha256'][:16]}…`" if row.get("sha256") else "CI 构建"
        add(f"| {row['platform']} | [`{row['file']}`]({base}/{row['file']}) | {row['size']} | {digest} |")
    add("")

    if extras["sidecars"] or (dist_dir / "SHA256SUMS.txt").exists():
        add("每个包旁边都有一个同名 `.sha256`（`sha256sum` 格式），汇总校验和在 `SHA256SUMS.txt`；"
            "包内另有 `build-info.json`（version / target / commit / rustc / 构建时间）。")
        add("")
        example = extras["sidecars"][0] if extras["sidecars"] else "<file>.sha256"
        add("```bash")
        add("# 单个包")
        add(f"sha256sum -c {example}")
        add("# 全部")
        add("sha256sum -c SHA256SUMS.txt --ignore-missing")
        add("```")
        add("")
        add("```powershell")
        add("Get-FileHash .\\flinkops-* -Algorithm SHA256   # 对照 SHA256SUMS.txt")
        add("```")
        add("")

    add("## 安装")
    add("")
    if "tar" in kinds:
        add("**Linux / macOS 压缩包**（解压即用，不需要安装）")
        add("")
        add("```bash")
        add(f"tar -xzf flinkops-{v}-<target>.tar.gz && cd flinkops-{v}-<target>")
        add("./start.sh                 # 等价于 ./flinkops；浏览器会自动打开 http://127.0.0.1:8080")
        add("# 可选：装到系统路径（默认 /usr/local，PREFIX 可改）")
        add("sudo ./install.sh")
        add("```")
        add("")
    if "deb" in kinds:
        add("**Debian / Ubuntu**")
        add("")
        add("```bash")
        add(f"sudo dpkg -i flinkops_{v}_amd64.deb        # arm64 用 flinkops_{v}_arm64.deb")
        add("sudoedit /etc/flinkops/flinkops.env      # FLINKOPS_CLUSTERS=prod=/etc/flinkops/kubeconfig")
        add("sudo systemctl enable --now flinkops")
        add("```")
        add("")
    if "appimage" in kinds:
        add("**AppImage**（单文件、免安装）")
        add("")
        add("```bash")
        add(f"chmod +x flinkops-{v}-x86_64-unknown-linux-gnu.AppImage")
        add(f"./flinkops-{v}-x86_64-unknown-linux-gnu.AppImage")
        add("# 无 FUSE 的环境（容器/精简系统）：APPIMAGE_EXTRACT_AND_RUN=1 ./flinkops-*.AppImage")
        add("```")
        add("")
    if "exe" in kinds or "zip" in kinds:
        add("**Windows**")
        add("")
        add("```powershell")
        if "exe" in kinds:
            add(f"# 安装包：双击 flinkops-{v}-x86_64-pc-windows-msvc-setup.exe（按用户安装，无需管理员）")
        if "zip" in kinds:
            add(f"# 便携版：解压 flinkops-{v}-x86_64-pc-windows-msvc.zip，双击 flinkops.exe（或 start.cmd）")
        add("& \"$env:LOCALAPPDATA\\Programs\\FlinkOps\\flinkops.exe\"    # 安装版路径")
        add("```")
        add("")
    add("**CLI / MCP**（三个二进制都在同一个包里）")
    add("")
    add("```bash")
    add("flinkopsctl doctor                        # 环境体检")
    add("flinkopsctl jobs list --url http://127.0.0.1:8080")
    add("flinkops-mcp                              # stdio MCP server，供 AI 客户端接入")
    add("```")
    add("")
    if "src" in kinds:
        add("**从源码构建**")
        add("")
        add("```bash")
        add(f"tar -xzf flinkops-{v}-src.tar.gz        # 或用 -src.zip")
        add(f"cd flinkops-{v}-src")
        add("cd apps/web && npm ci && npm run build && cd -")
        add("cargo build --release --bins -p flinkops-api -p flinkops-cli -p flinkops-mcp")
        add("FLINKOPS_WEB_DIR=apps/web/dist ./target/release/flinkops   # http://127.0.0.1:8080")
        add("```")
        add("")
        add("源码快照按 `git ls-files` 清单生成（受版本控制 + 未忽略的新文件），"
            "不含 `target/`、`dist/`、`node_modules/`；`Cargo.lock`、`apps/web/package-lock.json` 都在，"
            "打 tag 时即等于 tag 内容。")
        add("")

    add("## 容器镜像")
    add("")
    add("```bash")
    add(f"docker pull ghcr.io/state-flux/flinkops:{v}")
    add("docker run -d --name flinkops -p 8080:8080 -v flinkops-data:/data \\")
    add("  -v ~/.kube/config:/etc/flinkops/kubeconfig:ro \\")
    add('  -e FLINKOPS_CLUSTERS="prod=/etc/flinkops/kubeconfig" \\')
    add(f"  ghcr.io/state-flux/flinkops:{v}")
    add("```")
    add("")
    add("`linux/amd64` + `linux/arm64` 多架构构建，distroless 运行时（无 shell、非 root），"
        "`FLINKOPS_BIND` 已设为 `0.0.0.0:8080`。")
    add("")

    add("## Helm")
    add("")
    add("```bash")
    add("helm install flinkops oci://ghcr.io/state-flux/charts/flinkops \\")
    add(f"  --version {v} -n flinkops --create-namespace \\")
    add("  --set store.storageClass=local-path")
    add("```")
    add("")

    add("## 注意")
    add("")
    add("- 所有产物**未做代码签名**，Windows SmartScreen / macOS Gatekeeper 会提示；请先按上面的方式校验。")
    add("- 默认管理员 `admin/admin`，首次登录后立即改密；生产环境用 `FLINKOPS_ADMIN_*` 环境变量预置。")
    add("- 包内 `.env` 里是零配置默认值（`127.0.0.1:8080`、`web/`、`data/`、自动开浏览器），改完重启即可。")
    add(f"- 完整变更与已知问题见 [`CHANGELOG.md`](https://github.com/{repo}/blob/{tag}/CHANGELOG.md)。")
    add("")
    if full:
        add("> 本文件在本地生成（未推 tag），标 `CI 构建` 的行由 `dist` 矩阵在 runner 上产出，"
            "哈希以发布页的 `SHA256SUMS.txt` 为准。")
        add("")
    add(f"**Full Changelog**: https://github.com/{repo}/releases/tag/{tag}")
    add("")
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="生成 FlinkOps release notes（产物清单取自 scripts/dist.py）")
    ap.add_argument("--version", required=True)
    ap.add_argument("--tag")
    ap.add_argument("--dist", default=str(ROOT / "dist"))
    ap.add_argument("--repo", default="state-flux/flinkops")
    ap.add_argument("--out", default=str(ROOT / "dist" / "RELEASE_NOTES.md"))
    ap.add_argument("--print", dest="to_stdout", action="store_true")
    ap.add_argument(
        "--full-matrix",
        action="store_true",
        help="补齐本地没有、但 CI 会产出的产物（预览发行页用；CI 上不需要）",
    )
    args = ap.parse_args(argv)

    version = args.version.lstrip("v")
    tag = args.tag or f"v{version}"
    dist_dir = Path(args.dist)
    rows = collect(dist_dir, version)
    if not rows and not args.full_matrix:
        raise SystemExit(
            f"{dist_dir} 里没有 {version} 的产物：先跑 scripts/dist.py（或 make dist），"
            f"或加 --full-matrix 只看文案骨架"
        )
    if args.full_matrix:
        rows = fill_missing(rows, version)
    text = render(version, dist_dir, rows, tag, args.repo, args.full_matrix)

    if args.to_stdout:
        sys.stdout.write(text)
        return 0
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text, encoding="utf-8")
    print(f"[notes] {out}  ({len(rows)} 个产物)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
