// 确认弹窗的队列语义：连点两次不能把第一次的 Promise 覆盖掉，
// 否则会出现"点了取消却继续执行"这种最危险的错。
import { describe, expect, it } from "vitest";
import { confirmDialog, confirmState, promptDialog, settleCancel, settleConfirm } from "../confirm";

async function drain<T>(p: Promise<T>): Promise<T> {
  // 弹窗是"打开后等 settle"，这里把当前这一条立即结掉
  await Promise.resolve();
  return p;
}

describe("confirm queue", () => {
  it("confirm 打开后才 resolve，取消返回 false", async () => {
    const p = confirmDialog({ message: "确认？" });
    await Promise.resolve();
    expect(confirmState.open).toBe(true);
    expect(confirmState.mode).toBe("confirm");
    settleCancel();
    await expect(p).resolves.toBe(false);
    expect(confirmState.open).toBe(false);
  });

  it("确认返回 true", async () => {
    const p = confirmDialog({ message: "继续？", danger: true });
    await Promise.resolve();
    settleConfirm(true);
    await expect(p).resolves.toBe(true);
  });

  it("prompt 返回输入值，取消返回 null", async () => {
    const p1 = promptDialog({ message: "名字？", input: { value: "abc" } });
    await Promise.resolve();
    expect(confirmState.mode).toBe("prompt");
    expect(confirmState.opts.input?.value).toBe("abc");
    settleConfirm("abc");
    await expect(p1).resolves.toBe("abc");

    const p2 = promptDialog({ message: "名字？", input: {} });
    await Promise.resolve();
    settleCancel();
    await expect(p2).resolves.toBeNull();
  });

  it("排队：两次调用按顺序各自拿到结果", async () => {
    const first = confirmDialog({ message: "第一次" });
    const second = confirmDialog({ message: "第二次" });
    await Promise.resolve();

    // 第一个先结掉，第二个才会被推到前台
    settleConfirm(true);
    await expect(drain(first)).resolves.toBe(true);
    await Promise.resolve();
    expect(confirmState.opts.message).toBe("第二次");
    settleCancel();
    await expect(drain(second)).resolves.toBe(false);
  });
});
