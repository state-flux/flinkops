// 统一的"当前用户 / 是否只读"来源。
//
// 后端在只读模式下会对写接口返回 403，前端如果在按钮上不禁用，用户要先点一次
// 才知道 —— 演练/割接期间很容易被当成故障。所有写按钮都应该用 useWritable()。
import { computed } from "vue";
import { useQuery } from "@tanstack/vue-query";
import { auth } from "../api/client";

export function useMe() {
  return useQuery({
    queryKey: ["me"],
    queryFn: auth.me,
    retry: false,
    staleTime: 60_000,
  });
}

export function useWritable() {
  const me = useMe();
  const readonly = computed(
    () => !!(me.data?.value as { readonly?: boolean } | undefined)?.readonly,
  );
  return { me, readonly };
}
