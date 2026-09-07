/** 上游协议族。决定用哪套转换器与请求路径。 */
export type UpstreamType =
  | "claude"
  | "openai"
  | "gemini"
  | "codex"
  | "deepseek";

/** 密钥被踢出候选集的原因。硬状态需外部信号才恢复，冷却到期自动恢复。 */
export type HardState = "quota_exhausted" | "auth_invalid" | "banned";

/** 接入点：一个上游地址 + 它支持的模型集合。 */
export interface ApiEndpoint {
  id: string;
  name: string;
  upstreamType: UpstreamType;
  baseUrl: string;
  /** 空数组表示不限模型，任何 model 都可命中。 */
  models: string[];
  /** 层级优先级，越小越优先。与 sortIndex（展示序）解耦。 */
  priority: number;
  enabled: boolean;
  sortIndex?: number;
  notes?: string;
  createdAt: number;
  keyCount: number;
}

export interface NewApiEndpoint {
  name: string;
  upstreamType: UpstreamType;
  baseUrl: string;
  models?: string[];
  priority?: number;
  notes?: string;
}

/** 密钥记录。明文不下发，只有末四位。 */
export interface ApiKey {
  id: string;
  endpointId: string;
  keyLast4: string;
  name?: string;
  internalPriority: number;
  enabled: boolean;
  /** LRU 轮询依据。null 表示从未使用，排最前。 */
  lastUsedAt: number | null;
  cooldownUntil: number | null;
  cooldownReason?: string;
  hardState: HardState | null;
  requestCount: number;
  successCount: number;
  errorCount: number;
  totalTokens: number;
  totalCostUsd: number;
  lastErrorAt: number | null;
  lastErrorMessage?: string;
  createdAt: number;
}

export interface NewApiKey {
  endpointId: string;
  apiKey: string;
  name?: string;
  internalPriority?: number;
}

/** 选线候选，按生效顺序排列。 */
export interface RouteCandidate {
  keyId: string;
  endpointId: string;
  endpointName: string;
  baseUrl: string;
  upstreamType: UpstreamType;
  keyLast4: string;
  priority: number;
  internalPriority: number;
  lastUsedAt: number | null;
}

/** 上游类型展示名。 */
export const UPSTREAM_LABELS: Record<UpstreamType, string> = {
  codex: "Codex API",
  openai: "OpenAI 兼容",
  deepseek: "DeepSeek",
  claude: "Claude",
  gemini: "Gemini",
};

/** 侧栏展示顺序。 */
export const UPSTREAM_ORDER: UpstreamType[] = [
  "codex",
  "openai",
  "deepseek",
  "claude",
  "gemini",
];
