import { describe, expect, it } from "vitest";
import {
  extractPrompts,
  formatBody,
} from "@/components/requestLogs/traceFormatting";

describe("request trace prompt extraction", () => {
  it("preserves Chat system/developer prompts and tool outputs verbatim", () => {
    const segments = extractPrompts(
      JSON.stringify({
        messages: [
          { role: "system", content: " 系统\n提示词 " },
          { role: "developer", content: "开发者约束" },
          {
            role: "user",
            content: [
              { type: "text", text: "<script>中文</script>" },
              {
                type: "image_url",
                image_url: { url: "data:image/png;base64,LARGE" },
              },
            ],
          },
          { role: "tool", content: "工具结果", tool_call_id: "call_1" },
        ],
      }),
    );
    expect(segments).toEqual([
      { role: "system", text: " 系统\n提示词 " },
      { role: "developer", text: "开发者约束" },
      { role: "user", text: "<script>中文</script>\n[image_url]" },
      { role: "tool", text: "工具结果" },
    ]);
  });
  it("handles Responses instructions, string input and tool output items", () => {
    expect(extractPrompts('{"instructions":"规则","input":"问题"}')).toEqual([
      { role: "system", text: "规则" },
      { role: "user", text: "问题" },
    ]);
    expect(
      extractPrompts(
        '{"input":[{"type":"function_call_output","output":"结果"}]}',
      ),
    ).toEqual([{ role: "function_call_output", text: "结果" }]);
  });
  it("handles Anthropic system blocks and tool result blocks", () => {
    const body = {
      system: [{ type: "text", text: "系统" }],
      messages: [
        { role: "user", content: [{ type: "tool_result", content: "结果" }] },
      ],
    };
    expect(extractPrompts(JSON.stringify(body))).toEqual([
      { role: "system", text: "系统" },
      { role: "user", text: "结果" },
    ]);
  });
  it("does not fabricate text from truncated or malformed payloads", () => {
    expect(extractPrompts('{"input":"half')).toBeNull();
    const raw = '{\n  "input":"中文"\n}';
    expect(formatBody(raw, false)).toBe(raw);
    expect(formatBody("data: [DONE]\n\n", true)).toBe("data: [DONE]\n\n");
  });
});
