// 最小 localStorage polyfill：i18n 在模块加载时就会读它，
// 所以必须在 import 之前把全局补好（vitest setupFiles 满足这个时机）。
const store = new Map<string, string>();

const localStorageMock = {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, String(v)),
  removeItem: (k: string) => void store.delete(k),
  clear: () => store.clear(),
  key: (i: number) => [...store.keys()][i] ?? null,
  get length() {
    return store.size;
  },
};

(globalThis as unknown as { localStorage: typeof localStorageMock }).localStorage =
  localStorageMock;
