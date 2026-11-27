// 时间显示：后端给 UTC，界面必须按本地时区换算。
//
// 这里不假装"某个固定时区"——断言写成"与 Date 的本地取值一致"，
// 这样在 CI（UTC）和本机（Asia/Shanghai）都成立，同时能抓住"直接截字符串"的回归。
import { describe, expect, it } from "vitest";
import { fmtLocal, fmtLocalShort, tzLabel, toDate, utcOf } from "../time";

describe("time", () => {
  it("RFC3339 的 UTC 串按本地时区渲染，而不是原样截断", () => {
    const iso = "2026-11-27T01:18:31Z";
    const d = new Date(iso);
    const expected =
      `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")} ` +
      `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}:${String(d.getSeconds()).padStart(2, "0")}`;
    expect(fmtLocal(iso)).toBe(expected);
    // 反例：旧实现会把 UTC 的 01:18 直接贴出来（UTC 环境下两者本就相等，跳过）
    if (new Date().getTimezoneOffset() !== 0) {
      expect(fmtLocal(iso)).not.toBe("2026-11-27 01:18:31");
    }
  });

  it("秒级与毫秒级时间戳等价", () => {
    const ms = 1_700_000_000_000;
    expect(fmtLocal(ms)).toBe(fmtLocal(ms / 1000));
    expect(fmtLocal(String(ms))).toBe(fmtLocal(ms));
  });

  it("空值与垃圾输入给占位符，不抛异常", () => {
    for (const v of [null, undefined, "", "  ", "not-a-date", Number.NaN]) {
      expect(fmtLocal(v)).toBe("—");
      expect(utcOf(v)).toBe("—");
    }
  });

  it("toDate 能解 Date / RFC3339 / 时间戳", () => {
    const d = new Date("2026-11-27T01:18:31Z");
    expect(toDate(d)?.getTime()).toBe(d.getTime());
    expect(toDate("2026-11-27T01:18:31Z")?.getTime()).toBe(d.getTime());
    expect(toDate(d.getTime())?.getTime()).toBe(d.getTime());
  });

  it("utcOf 返回带 Z 的原始时间，便于和本地时间对照", () => {
    expect(utcOf("2026-11-27T01:18:31Z")).toBe("2026-11-27T01:18:31Z");
  });

  it("fmtLocalShort 是 MM-DD HH:mm", () => {
    expect(fmtLocalShort("2026-11-27T01:18:31Z")).toMatch(/^\d{2}-\d{2} \d{2}:\d{2}$/);
  });

  it("tzLabel 形如 UTC+08:00 / UTC-05:00 / UTC+00:00", () => {
    expect(tzLabel()).toMatch(/^UTC[+-]\d{2}:\d{2}$/);
  });
});
