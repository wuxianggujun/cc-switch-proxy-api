export type ApiEndpointUrlError =
  | "required"
  | "invalid"
  | "httpOnly"
  | "credentials"
  | "queryOrFragment";

export function normalizeApiEndpointUrl(raw: string): string {
  return raw.trim().replace(/\/+$/, "");
}

export function validateApiEndpointUrl(
  raw: string,
): ApiEndpointUrlError | null {
  const value = raw.trim();
  if (!value) return "required";

  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return "invalid";
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return "httpOnly";
  }
  if (!url.hostname) return "invalid";
  if (url.username || url.password) return "credentials";
  if (value.includes("?") || value.includes("#")) return "queryOrFragment";
  return null;
}
