export interface RequestTraceConfig {
  enabled: boolean;
  captureBodies: boolean;
  maxBodyBytes: number;
  retentionDays: number;
  maxEntries: number;
  maxStorageMb: number;
}

export interface TracePayload {
  headers: Array<{ name: string; value: string }>;
  body: string;
  bodyBytes: number;
  capturedBytes: number;
  truncated: boolean;
  redacted: boolean;
  bodyEncoding: "utf8" | "base64" | "omitted";
  captureError: string | null;
}

export interface RequestTraceSummary {
  requestId: string;
  startedAt: number;
  method: string;
  path: string;
  clientIp: string;
  clientPort: number | null;
  entryProtocol: string;
  model: string | null;
  statusCode: number | null;
  state: "in_progress" | "completed" | "error" | "cancelled" | "interrupted";
  durationMs: number | null;
  firstByteMs: number | null;
  attemptCount: number;
  providerName: string | null;
}

export interface RequestTraceAttempt {
  index: number;
  providerId: string;
  providerName: string;
  protocol: string;
  method: string;
  url: string;
  proxy: string | null;
  model: string | null;
  startedAt: number;
  durationMs: number | null;
  statusCode: number | null;
  error: string | null;
  request: TracePayload;
  response: TracePayload | null;
}

export interface RequestTraceDetail extends RequestTraceSummary {
  request: TracePayload;
  response: TracePayload | null;
  attempts: RequestTraceAttempt[];
  error: string | null;
  bodyCaptureEnabled: boolean;
}

export interface RequestTraceFilters {
  query?: string;
  clientIp?: string;
  entryProtocol?: string;
  statusCode?: number;
  errorsOnly?: boolean;
  startTime?: number;
  endTime?: number;
}

export interface RequestTracePage {
  data: RequestTraceSummary[];
  total: number;
  storageBytes: number;
}
