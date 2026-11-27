#!/usr/bin/env python3
"""把发行产物整理成「可直接上传」的一包。

产出（默认在 `dist/`）：

    MANIFEST.md                             附件清单：文件名 / 大小 / SHA-256 / 平台 / 用途
    UPLOAD.md                               上传步骤（拖哪些文件、正文贴哪个、怎么标 latest）
    flinkops-<ver>-upload-bundle.zip        上面这些 + 全部附件打成一个包，方便搬机器/一次性上传

产物清单同样取自 `scripts/dist.py`（`artifact_name()`），不会和打包器漂移；
本地还没产出的平台（例如 Linux 二进制只能在 CI 上编）会在清单里单独列成「待 CI 补齐」。

用法::

    python scripts/upload_bundle.py --version 0.1.0                 # dist/ 里的产物
    python scripts/upload_bundle.py --version 0.1.0 --dist release  # 指定目录
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def _load(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:  # pragma: no cover
        raise SystemExit(f"无法导入 {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


rn = _load("flinkops_release_notes", ROOT / "scripts" / "release_notes.py")

UPLOADABLE_SUFFIXES = (".tar.gz", ".zip", ".deb", ".appimage", ".exe")


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


def is_package(path: Path) -> bool:
    name = path.name.lower()
    if name.endswith(".sha256") or name.endswith(".build-info.json"):
        return False
    if name in ("sha256sums.txt", "release_notes.md", "manifest.md", "upload.md"):
        return False
    if name.endswith(".zip") and "upload-bundle" in name:
        return False
    return name.startswith("flinkops") and name.endswith(UPLOADABLE_SUFFIXES)


def collect(dist_dir: Path, version: str):
    planned_rows = rn.planned(version)
    plans = {row["file"]: row for row in planned_rows}
    order = {row["file"]: i for i, row in enumerate(planned_rows)}
    packages, sidecars, planned_missing, build_info = [], {}, [], []
    for path in sorted(dist_dir.iterdir()):
        if not path.is_file():
            continue
        if path.name.endswith(".build-info.json") and version in path.name:
            build_info.append(path.name)
            continue
        if is_package(path):
            row = plans.get(path.name, {"platform": "其它", "kind": ""})
            packages.append(
                {
                    "file": path.name,
                    "platform": row["platform"],
                    "kind": row.get("kind", ""),
                    "size": human(path.stat().st_size),
                    "sha256": sha256(path),
                }
            )
        elif path.name.endswith(".sha256"):
            sidecars[path.name[: -len(".sha256")]] = path.name
    packages.sort(key=lambda p: order.get(p["file"], 999))
    have = {p["file"] for p in packages}
    for name, row in plans.items():
        if name not in have:
            planned_missing.append({"file": name, "platform": row["platform"]})
    return packages, sidecars, planned_missing, build_info


def render_manifest(version: str, tag: str, repo: str, packages, sidecars, missing, build_info, sums_present: bool) -> str:
    out = [f"# FlinkOps {tag} 附件清单", ""]
    out.append(f"本地共 **{len(packages)}** 个可上传附件"
               + (f"，另有 {len(sidecars)} 个 `.sha256` 与 `SHA256SUMS.txt`" if sums_present else "")
               + "。")
    out.append("")
    out.append("| 平台 | 文件 | 大小 | SHA-256 |")
    out.append("|---|---|---|---|")
    for row in packages:
        out.append(f"| {row['platform']} | `{row['file']}` | {row['size']} | `{row['sha256']}` |")
    out.append("")
    if sidecars:
        out.append("## 校验附属文件")
        out.append("")
        for target, side in sorted(sidecars.items()):
            out.append(f"- `{side}` → 校验 `{target}`")
        out.append("")
    for name in build_info:
        out.append(f"- `{name}` → 构建元数据（version / target / commit / rustc / 构建时间），一起上传")
    if build_info:
        out.append("")
    if sums_present:
        out.append("- `SHA256SUMS.txt` → 一次校验全部附件（`sha256sum -c SHA256SUMS.txt --ignore-missing`）")
        out.append("")
    if missing:
        out.append("## 待 CI 补齐（本地编不出来）")
        out.append("")
        out.append("下列产物由 `dist` 矩阵在 GitHub runner 上构建，本地不会出现在包里：")
        out.append("")
        for row in missing:
            out.append(f"- `{row['file']}` —— {row['platform']}")
        out.append("")
    out.append(f"发布页：https://github.com/{repo}/releases/tag/{tag}")
    out.append("")
    return "\n".join(out)


def render_upload_howto(version: str, tag: str, repo: str, packages, missing) -> str:
    names = "\n".join(f"  {p['file']}" for p in packages)
    out = [
        f"# 上传 {tag} 的步骤",
        "",
        "## 方式 A（推荐）：推 tag 让 CI 出全套",
        "",
        "```bash",
        f"git push origin main --follow-tags      # 推 main 与 v{version} tag",
        "gh run watch                            # 观察 release 流水线",
        f"gh release view {tag}                    # 12 个产物由 CI 自动挂好",
        "```",
        "",
        "CI 会在原生 runner 上编出 Linux（tar.gz / deb / AppImage × amd64,arm64）、macOS、Windows、",
        "源码包与 Helm chart，并自动写好正文、算好校验和 —— 本地编不出的平台靠它补齐。",
        "",
        "## 方式 B（手动上传本地这包）",
        "",
        f"1. 打开 https://github.com/{repo}/releases/new?tag={tag}",
        f"   （或已有 Release 页面 → Edit → 附件区拖拽）",
        "2. 把下面这些文件**全部**拖进附件区：",
        "",
        "```",
        names,
        "```",
        "",
        "   以及对应的 `.sha256`、`.build-info.json` 和 `SHA256SUMS.txt`（都在同一个目录里）。",
        "",
        "3. 正文：把本包里的 `RELEASE_NOTES.md` 内容整体粘贴进描述框",
        "   （**不要**把 `RELEASE_NOTES.md` 当附件上传）。",
        f"4. 标题填 `FlinkOps {version}`，正式版勾选 *Set as the latest release*（预发布则勾 pre-release）。",
        "5. 发布后本地复核：",
        "",
        "```bash",
        "sha256sum -c SHA256SUMS.txt --ignore-missing",
        "```",
        "",
    ]
    if missing:
        out += [
            "## 注意",
            "",
            f"本地只编得出 {len(packages)} 个包；Linux / macOS 等 "
            f"{len(missing)} 个产物需要 CI（或在一台 Linux 机器上跑 `make dist-linux`）。",
            "手动上传时正文里的这些行会 404，建议直接用方式 A。",
            "",
        ]
    out += [
        "## 包内结构（每个平台包一致）",
        "",
        "```",
        "flinkops-<ver>-<target>/",
        "├── flinkops[.exe]        控制面 + Web UI（双击/直接运行）",
        "├── flinkopsctl[.exe]     CLI",
        "├── flinkops-mcp[.exe]    MCP server",
        "├── web/                  前端静态资源",
        "├── .env                  零配置：127.0.0.1:8080 / web ./web / data ./data",
        "├── start.cmd | start.sh  启动器（可选）",
        "├── install.ps1 | install.sh  装到系统路径（可选；Windows 免管理员）",
        "└── LICENSE NOTICE README.md VERSION build-info.json",
        "```",
        "",
    ]
    return "\n".join(out)


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="把发行产物整理成可上传的一包")
    ap.add_argument("--version", required=True)
    ap.add_argument("--tag")
    ap.add_argument("--repo", default="state-flux/flinkops")
    ap.add_argument("--dist", default=str(ROOT / "dist"))
    ap.add_argument("--notes", help="发行说明正文，默认 <dist>/RELEASE_NOTES.md")
    ap.add_argument("--out", help="输出 zip，默认 <dist>/flinkops-<ver>-upload-bundle.zip")
    args = ap.parse_args(argv)

    version = args.version.lstrip("v")
    tag = args.tag or f"v{version}"
    dist_dir = Path(args.dist)
    if not dist_dir.exists():
        raise SystemExit(f"{dist_dir} 不存在：先跑 scripts/dist.py 打包")

    notes = Path(args.notes) if args.notes else dist_dir / "RELEASE_NOTES.md"
    sums = dist_dir / "SHA256SUMS.txt"
    packages, sidecars, missing, build_info = collect(dist_dir, version)
    if not packages:
        raise SystemExit(f"{dist_dir} 里没有可上传的产物：先跑 scripts/dist.py")

    manifest = render_manifest(version, tag, args.repo, packages, sidecars, missing, build_info, sums.exists())
    howto = render_upload_howto(version, tag, args.repo, packages, missing)
    (dist_dir / "MANIFEST.md").write_text(manifest, encoding="utf-8")
    (dist_dir / "UPLOAD.md").write_text(howto, encoding="utf-8")

    out_zip = Path(args.out) if args.out else dist_dir / f"flinkops-{version}-upload-bundle.zip"
    stage = f"flinkops-{version}-upload"
    with zipfile.ZipFile(out_zip, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zf:
        for doc, text in (
            ("MANIFEST.md", manifest),
            ("UPLOAD.md", howto),
            ("RELEASE_NOTES.md", notes.read_text(encoding="utf-8") if notes.exists() else ""),
        ):
            zf.writestr(f"{stage}/{doc}", text)
        for path in [dist_dir / p["file"] for p in packages]:
            zf.write(path, f"{stage}/{path.name}")
        for side in sidecars.values():
            zf.write(dist_dir / side, f"{stage}/{side}")
        for name in build_info:
            zf.write(dist_dir / name, f"{stage}/{name}")
        if sums.exists():
            zf.write(sums, f"{stage}/SHA256SUMS.txt")

    print(f"[bundle] {out_zip}  ({human(out_zip.stat().st_size)})")
    print(f"[bundle] 附件 {len(packages)} 个 + sidecar {len(sidecars)} 个 + MANIFEST.md / UPLOAD.md")
    if missing:
        print(f"[bundle] 待 CI 补齐：{', '.join(m['file'] for m in missing)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
