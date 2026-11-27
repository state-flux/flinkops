// 作业状态语义的唯一判定点：这四条函数被顶栏、列表、KPI 共用，
// 一旦判定漂移就会出现"顶栏说需要关注、页面里一切正常"的自相矛盾。
import { describe, expect, it } from "vitest";
import { isFinished, isRisk, isSessionJob, isSuspended } from "../jobFilters";

describe("jobFilters", () => {
  it("FAILED 或 red 都算风险", () => {
    expect(isRisk({ state: "FAILED" })).toBe(true);
    expect(isRisk({ state: "RUNNING", health: "red" })).toBe(true);
    expect(isRisk({ state: "RUNNING", health: "yellow" })).toBe(false);
  });

  it("CANCELED / SUSPENDED 不算失败（用户主动停的不该进告警）", () => {
    expect(isRisk({ state: "CANCELED", health: "yellow" })).toBe(false);
    expect(isRisk({ state: "SUSPENDED", health: "any" })).toBe(false);
  });

  it("SUSPENDED 只由 state 决定", () => {
    expect(isSuspended({ state: "SUSPENDED" })).toBe(true);
    expect(isSuspended({ state: "RUNNING" })).toBe(false);
    expect(isSuspended({})).toBe(false);
  });

  it("批作业两个终态都算跑完", () => {
    expect(isFinished({ state: "FINISHED" })).toBe(true);
    expect(isFinished({ state: "SUCCEEDED" })).toBe(true);
    expect(isFinished({ state: "RUNNING" })).toBe(false);
  });

  it("会话作业靠 kind 区分（列表里两种 kind 混在一起）", () => {
    expect(isSessionJob({ kind: "FlinkSessionJob" })).toBe(true);
    expect(isSessionJob({ kind: "FlinkDeployment" })).toBe(false);
    expect(isSessionJob({})).toBe(false);
  });
});
