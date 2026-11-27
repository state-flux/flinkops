#!/usr/bin/env python3
"""把结果集 JSON 转成 xlsx，二进制写到 stdout。

用法：
    echo '{"columns":["a","b"],"rows":[[1,2]],"sheet":"result"}' | python to_xlsx.py > out.xlsx

为什么走脚本：Rust 侧没有 xlsx 依赖，而 Python 环境已有 openpyxl
（与 sync_catalog.py 复用同一套运行时，不额外引入 crate）。
"""
import io
import json
import sys


def main() -> int:
    try:
        from openpyxl import Workbook
        from openpyxl.styles import Font
    except ImportError:
        sys.stderr.write("缺少 openpyxl：python -m pip install openpyxl\n")
        return 3

    try:
        data = json.load(sys.stdin)
    except Exception as e:  # noqa: BLE001
        sys.stderr.write(f"stdin 不是合法 JSON：{e}\n")
        return 2

    columns = data.get("columns") or []
    rows = data.get("rows") or []
    sheet = (data.get("sheet") or "result")[:31] or "result"

    wb = Workbook()
    ws = wb.active
    ws.title = sheet

    if columns:
        ws.append([str(c) for c in columns])
        for cell in ws[1]:
            cell.font = Font(bold=True)
        # 列宽：按前 200 行估算，避免全表扫描
        for idx, name in enumerate(columns, start=1):
            width = len(str(name))
            for r in rows[:200]:
                if idx - 1 < len(r):
                    width = max(width, len(str(r[idx - 1])))
            ws.column_dimensions[ws.cell(row=1, column=idx).column_letter].width = min(
                48, max(10, width + 2)
            )

    for r in rows:
        ws.append([None if v is None else v for v in r])

    # 冻结首行，方便看大表
    if columns:
        ws.freeze_panes = "A2"

    buf = io.BytesIO()
    wb.save(buf)
    sys.stdout.buffer.write(buf.getvalue())
    return 0


if __name__ == "__main__":
    sys.exit(main())
