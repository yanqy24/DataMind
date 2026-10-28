/* ============================================================
 * types.ts — 前后端共享数据结构（与 Rust 侧 serde 定义保持一致）
 * ============================================================ */

/** SSH 隧道配置 */
export interface SshConfig {
  enabled: boolean;
  host: string;
  port: number;
  user: string;
  auth: "password" | "key";
  /** 密码 或 私钥文件路径（取决于 auth） */
  secret: string;
}

/** 数据库连接配置（本地持久化） */
export interface ConnConfig {
  id: string;
  name: string;
  /** 目前仅 "MySQL"，为后续拓展 PostgreSQL 等预留 */
  type: "MySQL";
  host: string;
  port: number;
  user: string;
  password: string;
  ssh: SshConfig;
}

/** 列元信息 */
export interface ColumnMeta {
  name: string;
  /** MySQL 数据类型，如 bigint / varchar(32) */
  type: string;
  comment: string;
}

/** 表元信息（本地记忆的一部分） */
export interface TableMeta {
  name: string;
  comment: string;
  /** 近似行数（information_schema 统计值） */
  rows: number;
  /** SHOW CREATE TABLE 结果 */
  ddl: string;
  columns: ColumnMeta[];
  /** 取样数据，行 -> {列名: 值} */
  sample: Record<string, unknown>[];
}

/** 表间关系（外键推导） */
export interface RelationMeta {
  /** orders.user_id */
  from: string;
  /** users.id */
  to: string;
  desc: string;
}

/** 数据库（schema）元信息 */
export interface DatabaseMeta {
  name: string;
  comment: string;
  tables: TableMeta[];
  relations: RelationMeta[];
}

/** 一个连接的完整本地记忆 */
export interface ConnMemory {
  connId: string;
  syncedAt: string;
  databases: DatabaseMeta[];
}

/** 记忆同步摘要（用于树上的徽标） */
export interface SyncSummary {
  tables: number;
  relations: number;
  databases: number;
  syncedAt: string;
}

/** 模型供应商预设 */
export type Provider = "deepseek" | "zhipu" | "custom";

export const PROVIDERS: Record<
  Provider,
  { name: string; baseUrl: string; models: string[] }
> = {
  deepseek: {
    name: "DeepSeek",
    baseUrl: "https://api.deepseek.com",
    models: ["deepseek-chat", "deepseek-reasoner"],
  },
  zhipu: {
    name: "智谱 AI",
    baseUrl: "https://open.bigmodel.cn/api/paas/v4",
    models: ["glm-4.6", "glm-4.5-air", "glm-4-flash"],
  },
  custom: { name: "自定义", baseUrl: "", models: [] },
};

/** 模型配置（本地持久化） */
export interface ModelConfig {
  id: string;
  name: string;
  provider: Provider;
  apiKey: string;
  modelId: string;
  baseUrl: string;
  active: boolean;
}

/** SQL 执行结果集 */
export interface QueryResult {
  columns: { name: string; type: string }[];
  /** 行 -> 单元格值（已转字符串，NULL 为 null） */
  rows: (string | null)[][];
  ms: number;
  truncated: boolean;
}

/** SQL 卡片状态 */
export type SqlStatus = "idle" | "running" | "success" | "error" | "cancelled";

/** SQL 执行错误详情 */
export interface SqlError {
  code: string;
  message: string;
  /** 出错片段 */
  pos?: string;
  /** 修复建议 */
  hint?: string;
}

/** 查询窗口内的一条 SQL */
/** 两阶段生成流水线的阶段留痕 */
export interface AgentStep {
  key: string; // schema | explore | design | build | verify
  label: string;
  summary: string;
  ok: boolean;
}

export interface SqlItem {
  id: string;
  sql: string;
  status: SqlStatus;
  error: SqlError | null;
  ms: number | null;
  rows: number | null;
  ts: string;
  /** 是否为「快速修复」生成的卡片 */
  fixed?: boolean;
  /** 是否为手写 / 粘贴创建的卡片 */
  manual?: boolean;
  /** 生成过程（分析 / 探查 / 设计 / 生成 / 校验）留痕 */
  trace?: AgentStep[];
}

/** 多轮上下文：传给后端的一轮对话（越靠后越新） */
export interface ChatTurn {
  role: "user" | "ai";
  text: string;
}

/** 对话消息 */
export interface ChatMessage {
  role: "user" | "ai";
  text: string;
  ts: string;
}

/** 查询窗口（Tab） */
export interface QueryTab {
  id: string;
  title: string;
  connId: string | null;
  db: string;
  table: string;
  /** 对话栏高度（px，持久化） */
  chatH: number | null;
  /** 对话栏左列宽度（px，持久化） */
  chatW: number | null;
  messages: ChatMessage[];
  sqls: SqlItem[];
  result: QueryResult | null;
  running: string | null;
}

/** SQL 日志条目 */
export interface SqlLogItem {
  id: string;
  ts: string;
  sql: string;
  status: SqlStatus;
  conn: string;
  db: string;
  ms: number | null;
}

/** 应用设置 */
export interface AppSettings {
  theme: "dark" | "light";
  /** SQL 执行超时（秒） */
  sqlTimeout: number;
  /** 两阶段 Agent 生成：先分析探查，再设计生成并校验 */
  agentMode: boolean;
}

/** 后端统一错误结构 */
export interface BackendError {
  code: string;
  message: string;
  hint: string | null;
}

/** 实时读取的单表详情（建表语句 + 取样） */
export interface TableDetail {
  ddl: string;
  sample: Record<string, string | null>[];
}
