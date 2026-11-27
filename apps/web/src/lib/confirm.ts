// 统一的"确认/输入"弹窗状态机。
//
// 原生 confirm()/prompt() 在不同浏览器里长得完全不一样（Chrome 还会顶一个
// "localhost:8080 显示"的标题），且无法表达"危险操作 / 详情 / 多行说明"。
// 这里用一个 Promise API 把调用点写成：
//
//   if (!(await confirmDialog({ title: "取消作业", message: "...", danger: true }))) return;
//   const name = await promptDialog({ message: "新名称", input: { value: "x" } });
//
// 渲染在 ConfirmHost.vue（挂在 App.vue），与全局 Modal 同一套视觉。
import { reactive } from "vue";

export type ConfirmInput = {
  label?: string;
  placeholder?: string;
  value?: string;
  /** 多行输入（YAML / 说明） */
  multiline?: boolean;
};

export type ConfirmOptions = {
  title?: string;
  /** 主文案，支持 \n（用 whitespace-pre-line 渲染） */
  message: string;
  /** 次级说明，弱化显示 */
  detail?: string;
  okText?: string;
  cancelText?: string;
  /** 危险操作：确认按钮变红 */
  danger?: boolean;
  input?: ConfirmInput;
};

type Resolver = (v: boolean | string | null) => void;

export const confirmState = reactive<{
  open: boolean;
  mode: "confirm" | "prompt";
  opts: ConfirmOptions;
  resolve: Resolver | null;
}>({
  open: false,
  mode: "confirm",
  opts: { message: "" },
  resolve: null,
});

let queue: Promise<unknown> = Promise.resolve();

/** 串行化：前一个弹窗没关，后一个排队（避免两次点击把 resolve 覆盖掉）。 */
function enqueue<T>(run: (done: (v: boolean | string | null) => void) => void): Promise<T> {
  const next = queue.then(
    () =>
      new Promise<T>((resolve) => {
        run((v) => {
          confirmState.open = false;
          confirmState.resolve = null;
          resolve(v as T);
        });
      }),
  );
  queue = next.catch(() => undefined);
  return next;
}

export function confirmDialog(opts: ConfirmOptions): Promise<boolean> {
  return enqueue<boolean>((done) => {
    confirmState.mode = "confirm";
    confirmState.opts = opts;
    confirmState.resolve = done;
    confirmState.open = true;
  });
}

export function promptDialog(
  opts: ConfirmOptions & { input: ConfirmInput },
): Promise<string | null> {
  return enqueue<string | null>((done) => {
    confirmState.mode = "prompt";
    confirmState.opts = opts;
    confirmState.resolve = done;
    confirmState.open = true;
  });
}

/** ConfirmHost 用：确认 */
export function settleConfirm(value: boolean | string | null) {
  confirmState.resolve?.(value);
}

/** ConfirmHost 用：取消（confirm → false，prompt → null） */
export function settleCancel() {
  confirmState.resolve?.(confirmState.mode === "prompt" ? null : false);
}
