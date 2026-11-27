import { computed, ref, watch } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { auth } from "../api/client";

const KEY = "flinkops.cluster";

/**
 * 全局"当前集群"。多集群时由 PageHead 右上角切换；
 * 各页面把它当作筛选默认值（用户仍可在页面内改）。
 */
const current = ref<string>(localStorage.getItem(KEY) ?? "");

watch(current, (v) => {
  if (v) localStorage.setItem(KEY, v);
  else localStorage.removeItem(KEY);
});

export function useCurrentCluster() {
  const { data: me } = useQuery({ queryKey: ["me"], queryFn: auth.me, retry: false });
  const clusters = computed(() => me.value?.clusters ?? []);

  // 只有一个集群时不显示切换器，但 current 仍返回该集群，方便页面直接用
  const effective = computed(() => {
    const list = clusters.value;
    if (!list.length) return "";
    if (current.value && list.includes(current.value)) return current.value;
    return list[0] ?? "";
  });

  return { current, effective, clusters, multi: computed(() => clusters.value.length > 1) };
}
