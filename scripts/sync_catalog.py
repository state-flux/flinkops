#!/usr/bin/env python3
"""把外部数据源的库表结构导成 JSON，供平台同步元数据 + 写 SQL 时补全。

用法：
    python sync_catalog.py "mysql://root:pass@host:9030/db"          # MySQL / StarRocks / Doris
    python sync_catalog.py "postgres://user:pass@host:5432/db"       # PostgreSQL
    python sync_catalog.py "mongodb://user:pass@host:27017/db"       # MongoDB（按集合统计字段）

输出（stdout，UTF-8）：
    {"ok": true, "tables": [{"db": "...", "table": "...", "columns": [{"name": "...", "type": "..."}]}]}
    {"ok": false, "error": "..."}   失败

驱动按 DSN scheme 自动选择，缺驱动时返回可执行的安装提示，不静默失败。

为什么走脚本而不是 Rust 直连：StarRocks 不认 sqlx 默认发的 `SET` 语句
（报 "Set statement only support constant expr"），而 pymysql 的握手是干净的。
"""
import json
import sys
from urllib.parse import urlparse, unquote

# 系统库，不同步
SYSTEM_DBS = {"information_schema", "mysql", "performance_schema", "sys", "_statistics_"}


def parse_dsn(dsn: str) -> dict:
    u = urlparse(dsn)
    if not u.scheme:
        raise ValueError(
            "DSN 缺少 scheme，示例：mysql://user:pass@host:9030/db 或 postgres://..."
        )
    if not u.hostname:
        raise ValueError("DSN 缺少主机名")
    return {
        "scheme": u.scheme.lower(),
        "host": u.hostname,
        "port": u.port,
        "user": unquote(u.username or "root"),
        "password": unquote(u.password or ""),
        "database": (u.path or "/").lstrip("/") or None,
    }


# ---------------------------------------------------------------- MySQL 家族


def fetch_mysql(cfg: dict):
    try:
        import pymysql
    except ImportError:
        raise RuntimeError("缺少 pymysql：python -m pip install pymysql")

    conn = pymysql.connect(
        host=cfg["host"],
        port=cfg["port"] or 9030,
        user=cfg["user"],
        password=cfg["password"],
        database=cfg["database"],
        charset="utf8mb4",
        connect_timeout=10,
        read_timeout=120,
        cursorclass=pymysql.cursors.DictCursor,
    )
    try:
        with conn.cursor() as cur:
            cur.execute(
                """
                SELECT TABLE_SCHEMA AS db, TABLE_NAME AS tbl,
                       COLUMN_NAME AS col, COLUMN_TYPE AS ctype
                FROM information_schema.COLUMNS
                ORDER BY TABLE_SCHEMA, TABLE_NAME, ORDINAL_POSITION
                """
            )
            return cur.fetchall()
    finally:
        conn.close()


# ---------------------------------------------------------------- PostgreSQL


def fetch_postgres(cfg: dict):
    try:
        import psycopg2
        import psycopg2.extras
    except ImportError:
        raise RuntimeError("缺少 psycopg2：python -m pip install psycopg2-binary")

    conn = psycopg2.connect(
        host=cfg["host"],
        port=cfg["port"] or 5432,
        user=cfg["user"],
        password=cfg["password"],
        dbname=cfg["database"] or "postgres",
        connect_timeout=10,
    )
    try:
        with conn.cursor(cursor_factory=psycopg2.extras.RealDictCursor) as cur:
            cur.execute(
                """
                SELECT table_schema AS db, table_name AS tbl,
                       column_name AS col, data_type AS ctype
                FROM information_schema.columns
                WHERE table_schema NOT IN ('pg_catalog', 'information_schema')
                ORDER BY table_schema, table_name, ordinal_position
                """
            )
            return cur.fetchall()
    finally:
        conn.close()


# ---------------------------------------------------------------- MongoDB


def fetch_mongodb(cfg: dict):
    """Mongo 没有 schema：采样一个文档，把字段路径 + bson 类型当列。"""
    try:
        from pymongo import MongoClient
    except ImportError:
        raise RuntimeError("缺少 pymongo：python -m pip install pymongo")

    cli = MongoClient(
        host=cfg["host"],
        port=cfg["port"] or 27017,
        username=cfg["user"] or None,
        password=cfg["password"] or None,
        serverSelectionTimeoutMS=10000,
    )
    rows = []
    try:
        db_names = [cfg["database"]] if cfg["database"] else cli.list_database_names()
        for db_name in db_names:
            if db_name in ("admin", "local", "config"):
                continue
            db = cli[db_name]
            for coll in db.list_collection_names():
                doc = db[coll].find_one() or {}
                for k, v in doc.items():
                    rows.append(
                        {"db": db_name, "tbl": coll, "col": k, "ctype": type(v).__name__}
                    )
    finally:
        cli.close()
    return rows


DRIVERS = {
    "mysql": fetch_mysql,      # MySQL / StarRocks / Doris 都是 MySQL 协议
    "starrocks": fetch_mysql,
    "doris": fetch_mysql,
    "postgres": fetch_postgres,
    "postgresql": fetch_postgres,
    "mongodb": fetch_mongodb,
    "mongo": fetch_mongodb,
}


def main() -> int:
    if len(sys.argv) < 2:
        print(json.dumps({"ok": False, "error": "缺少 DSN 参数"}), flush=True)
        return 2

    try:
        cfg = parse_dsn(sys.argv[1])
    except Exception as e:  # noqa: BLE001
        print(json.dumps({"ok": False, "error": str(e)}), flush=True)
        return 2

    driver = DRIVERS.get(cfg["scheme"])
    if driver is None:
        print(
            json.dumps(
                {
                    "ok": False,
                    "error": f"不支持的 scheme：{cfg['scheme']}；可用："
                    + ", ".join(sorted(set(DRIVERS.keys()))),
                }
            ),
            flush=True,
        )
        return 2

    try:
        rows = driver(cfg)
    except Exception as e:  # noqa: BLE001
        print(json.dumps({"ok": False, "error": f"{type(e).__name__}: {e}"}), flush=True)
        return 4

    grouped = {}
    for r in rows:
        db = r["db"]
        if db in SYSTEM_DBS:
            continue
        grouped.setdefault((db, r["tbl"]), []).append(
            {"name": r["col"], "type": r.get("ctype") or ""}
        )

    tables = [
        {"db": db, "table": tbl, "columns": cols}
        for (db, tbl), cols in sorted(grouped.items())
    ]
    print(
        json.dumps(
            {"ok": True, "scheme": cfg["scheme"], "tables": tables}, ensure_ascii=False
        ),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
