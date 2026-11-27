// 作业状态的"语义判定"只在这里定义一次。
//
// 顶栏、列表 tab、首页 KPI 都用它，避免出现"顶栏 4 个需关注、页面里只有 1 个"
// 这种自相矛盾（曾经就是顶栏还留着 health !== green 的老口径）。
export type JobLike = { state?: string; health?: string; kind?: string };

/** 真失败：用于"需要关注"计数与筛选 */
export const isRisk = (j: JobLike) => j.state === "FAILED" || j.health === "red";

/** 用户主动挂起（不是故障） */
export const isSuspended = (j: JobLike) => j.state === "SUSPENDED";

/** 正常终态：批作业跑完 */
export const isFinished = (j: JobLike) => j.state === "FINISHED" || j.state === "SUCCEEDED";

export const isSessionJob = (j: JobLike) => j.kind === "FlinkSessionJob";
