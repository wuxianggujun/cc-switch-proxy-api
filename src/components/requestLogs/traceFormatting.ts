export const protocolLabels: Record<string, string> = {
  anthropic: "Anthropic Messages",
  openai_chat: "OpenAI Chat",
  openai_responses: "OpenAI Responses",
  gemini_native: "Gemini Native",
  http: "HTTP",
};

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

export function formatBody(body: string, pretty: boolean): string {
  if (!pretty) return body;
  try {
    return JSON.stringify(JSON.parse(body), null, 2);
  } catch {
    return body;
  }
}

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function promptText(value: unknown): string {
  if (typeof value === "string") return value;
  if (value == null) return "";
  if (Array.isArray(value))
    return value.map(promptText).filter(Boolean).join("\n");
  const object = record(value);
  if (!object) return String(value);
  if (typeof object.text === "string") return object.text;
  if (typeof object.thinking === "string") return object.thinking;
  if (object.content != null) return promptText(object.content);
  if (
    typeof object.type === "string" &&
    /image|audio|file|document/.test(object.type)
  )
    return `[${object.type}]`;
  return JSON.stringify(value, null, 2);
}

export interface PromptSegment {
  role: string;
  text: string;
}
export function extractPrompts(body: string): PromptSegment[] | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return null;
  }
  const root = record(parsed);
  if (!root) return null;
  const result: PromptSegment[] = [];
  const append = (role: string, content: unknown) => {
    const text = promptText(content);
    if (text) result.push({ role, text });
  };
  append("system", root.system ?? root.instructions ?? root.systemInstruction);
  if (typeof root.input === "string") append("user", root.input);
  const messages = root.messages ?? root.input ?? root.contents;
  if (Array.isArray(messages))
    for (const value of messages) {
      const message = record(value);
      if (!message) continue;
      const role =
        typeof message.role === "string"
          ? message.role
          : String(message.type ?? "input");
      append(
        role,
        message.content ??
          message.parts ??
          message.output ??
          message.arguments ??
          message.input ??
          message.summary,
      );
      if (message.tool_calls) append("tool_calls", message.tool_calls);
      if (message.function_call) append("function_call", message.function_call);
    }
  return result;
}
