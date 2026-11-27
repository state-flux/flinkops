// 时间展示的唯一出口。
//
// 后端一律存/传 UTC（RFC3339 带 Z），界面必须按**浏览器本地时区**换算后再显示。
// 之前的写法是 `String(v).replace("T", " ").replace("Z", "")` —— 把 UTC 数字原样
// 当本地时间贴出来，北京时间会比实际早 8 小时，而且连 Z 都被抹掉了，看不出是 UTC。
//
// 约定：
//   fmtLocal(v)  → "2026-11-27 09:18:31"（本地时区，给用户看）
//   utcOf(v)     → "2026-11-27T01:18:31Z"（原始 UTC，放 title 里便于对照）
//   tzLabel()    → "UTC+08:00"（告诉用户当前按哪个时区显示）

/** 把后端返回值解析成 Date：支持 RFC3339 字符串、毫秒/秒时间戳、Date。 */
export function toDate(v: unknown): Date | null {
  if (v === null || v === undefined || v === "") return null;
  if (v instanceof Date) return Number.isNaN(v.getTime()) ? null : v;
  if (typeof v === "number") {
    // 10 位是秒、13 位是毫秒：后端快照里有秒级时间戳
    const ms = Math.abs(v) < 1e12 ? v * 1000 : v;
    const d = new Date(ms);
    return Number.isNaN(d.getTime()) ? null : d;
  }
  const s = String(v).trim();
  if (!s) return null;
  if (/^\d+$/.test(s)) return toDate(Number(s));
  const d = new Date(s);
  return Number.isNaN(d.getTime()) ? null : d;
}

const pad = (n: number, width = 2) => String(n).padStart(width, "0");

/** 本地时区的 `YYYY-MM-DD HH:mm:ss`；解析不了返回 "—"。 */
export function fmtLocal(v: unknown): string {
  const d = toDate(v);
  if (!d) return "—";
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
  );
}

/** 本地时区的 `MM-DD HH:mm`（表格窄列用）。 */
export function fmtLocalShort(v: unknown): string {
  const d = toDate(v);
  if (!d) return "—";
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 原始 UTC（ISO8601），适合放进 `title` 做对照。 */
export function utcOf(v: unknown): string {
  const d = toDate(v);
  return d ? d.toISOString().replace(".000Z", "Z") : "—";
}

/** 当前时区标签，如 `UTC+08:00`。 */
export function tzLabel(): string {
  const off = -new Date().getTimezoneOffset();
  const sign = off >= 0 ? "+" : "-";
  const abs = Math.abs(off);
  return `UTC${sign}${pad(Math.floor(abs / 60))}:${pad(abs % 60)}`;
}
