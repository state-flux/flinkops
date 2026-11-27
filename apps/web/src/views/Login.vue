<script setup lang="ts">
import { computed, ref } from "vue";
import { useRoute, useRouter } from "vue-router";
import { useQuery } from "@tanstack/vue-query";
import { auth } from "../api/client";
import { t } from "../lib/i18n";

const router = useRouter();
const route = useRoute();
const username = ref("admin");
const password = ref("");
// 开了两步验证的账号需要动态码；输入框在有需要时显示（先登录一次拿到错误码）
const totp = ref("");
const needTotp = ref(false);
const error = ref("");
const busy = ref(false);

// 免登录接口，用来在登录页展示环境信息
const ver = useQuery({
  queryKey: ["version"],
  queryFn: async () => {
    const r = await fetch("/api/version");
    return (await r.json()) as { version: string; clusters: number; metricsSource: string };
  },
  retry: false,
});

const features = [
  "作业生命周期：启停 / 重启 / Savepoint 都落到 CRD，可追溯",
  "实时指标走 JobManager REST，历史才用 Prometheus，指标不落库",
  "原生 Flink UI 走只读反代：cancel/stop 在代理层被拦截并审计",
];

async function submit() {
  busy.value = true;
  error.value = "";
  try {
    await auth.login(username.value, password.value, totp.value);
    const redirect = typeof route.query.redirect === "string" ? route.query.redirect : "/";
    await router.replace(redirect);
  } catch (e) {
    error.value = (e as Error).message;
    if (/两步验证|totp/i.test(error.value)) needTotp.value = true;
  } finally {
    busy.value = false;
  }
}

const env = computed(() => ver.data?.value);
</script>

<template>
  <div class="login-bg grid min-h-screen place-items-center px-6 py-10">
    <div
      class="grid w-full items-center gap-14"
      style="grid-template-columns: 1.05fr 400px; max-width: 1000px"
    >
      <!-- 左：品牌 -->
      <div class="login-brand hidden md:block">
        <div
          class="grid h-[46px] w-[46px] place-items-center rounded-[13px] bg-gradient-to-br from-[#ff9a8b] to-[#ff6b81] text-[22px] font-black text-[#0b1020]"
        >
          F
        </div>
        <h1 class="mt-[18px] mb-2 text-[30px] font-semibold tracking-tight text-[#0f172a]">
          {{ t("login.tagline") }}
        </h1>
        <p class="mb-[22px] text-sm2 text-[#475569]">
          {{ t("login.subtitle") }}
        </p>
        <ul class="flex list-none flex-col gap-[10px] p-0">
          <li
            v-for="f in features"
            :key="f"
            class="flex items-start gap-[9px] text-sm2 leading-relaxed text-[#334155]"
          >
            <span
              class="mt-[3px] h-[15px] w-[15px] shrink-0 rounded-full bg-gradient-to-br from-[#ff9a8b] to-[#ff6b81]"
            ></span>
            <span>{{ f }}</span>
          </li>
        </ul>
      </div>

      <!-- 右：白卡 -->
      <div class="rounded-[18px] bg-white px-7 pt-[30px] pb-6 shadow-[0_24px_60px_#0f172a26]">
        <h2 class="m-0 text-[17px] font-semibold text-[#0f172a]">{{ t("login.signIn") }}</h2>
        <p class="mt-1 mb-[18px] text-xs text-[#64748b]">
          {{ t("login.hint") }}
        </p>

        <form class="flex flex-col gap-3" @submit.prevent="submit">
          <label class="flex flex-col gap-1">
            <span class="text-2xs font-medium text-[#475569]">{{ t("login.username") }}</span>
            <input
              v-model="username"
              autocomplete="username"
              class="rounded-[9px] border border-[#e2e8f0] px-3 py-2 text-sm2 text-[#0f172a] outline-none focus:border-[#ff6b81] focus:ring-2 focus:ring-[#ff6b81]/15"
            />
          </label>
          <label class="flex flex-col gap-1">
            <span class="text-2xs font-medium text-[#475569]">{{ t("login.password") }}</span>
            <input
              v-model="password"
              type="password"
              autocomplete="current-password"
              :placeholder="t('login.usernamePlaceholder')"
              class="rounded-[9px] border border-[#e2e8f0] px-3 py-2 text-sm2 text-[#0f172a] outline-none focus:border-[#ff6b81] focus:ring-2 focus:ring-[#ff6b81]/15"
            />
          </label>

          <label v-if="needTotp" class="flex flex-col gap-1">
            <span class="text-2xs font-medium text-[#475569]">{{ t("login.totp") }}</span>
            <input
              v-model="totp"
              inputmode="numeric"
              autocomplete="one-time-code"
              maxlength="6"
              :placeholder="t('login.totpPlaceholder')"
              class="rounded-[9px] border border-[#e2e8f0] px-3 py-2 font-mono text-sm2 tracking-widest text-[#0f172a] outline-none focus:border-[#ff6b81] focus:ring-2 focus:ring-[#ff6b81]/15"
            />
          </label>

          <p v-if="error" class="rounded-[9px] bg-[#fef2f2] px-3 py-2 text-xs text-[#b91c1c]">
            {{ error }}
          </p>

          <button
            type="submit"
            :disabled="busy"
            class="mt-1 w-full rounded-[9px] bg-[#ff6b81] py-2.5 text-sm2 font-semibold text-white transition hover:bg-[#f4586f] disabled:opacity-60"
          >
            {{ busy ? "登录中…" : "登录" }}
          </button>
        </form>

        <div class="my-[14px] flex items-center gap-[10px] text-2xs text-[#9aa4b2]">
          <span class="h-px flex-1 bg-[#e2e8f0]"></span><span>{{ t("login.env") }}</span
          ><span class="h-px flex-1 bg-[#e2e8f0]"></span>
        </div>

        <div class="grid grid-cols-2 gap-2 text-2xs text-[#475569]">
          <div class="flex justify-between">
            <span class="text-[#94a3b8]">{{ t("login.version") }}</span><span class="font-mono">{{
              env?.version ?? "—"
            }}</span>
          </div>
          <div class="flex justify-between">
            <span class="text-[#94a3b8]">{{ t("shell.cluster") }}</span><span class="font-mono">{{
              env?.clusters ?? "—"
            }}</span>
          </div>
          <div class="col-span-2 flex justify-between">
            <span class="text-[#94a3b8]">{{ t("login.metricsSource") }}</span><span
              class="font-mono"
              >{{ env?.metricsSource ?? "—" }}</span
            >
          </div>
        </div>
      </div>
    </div>
  </div>
</template>
