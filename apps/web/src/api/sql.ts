// SQL 工作台：单独成文件，避免 client.ts 过大导致打包器解析不到导出
import { request } from "./client";

export type SqlExecResult = {
  ok: boolean;
  gateway: string;
  columns: string[];
  rows: unknown[][];
  rowCount: number;
  truncated: boolean;
  tookMs: number;
};

export const sqlApi = {
  execute: (body: {
    cluster?: string;
    namespace?: string;
    session: string;
    sql: string;
    maxRows?: number;
  }) => request<SqlExecResult>("/api/sql/execute", { method: "POST", body: JSON.stringify(body) }),
  gatewayInfo: (cluster: string, ns: string, session: string) =>
    request<{
      found: boolean;
      up?: boolean;
      url?: string;
      info?: Record<string, unknown>;
      error?: string;
    }>(`/api/sql/gateway/${cluster}/${ns}/${session}`),
};
