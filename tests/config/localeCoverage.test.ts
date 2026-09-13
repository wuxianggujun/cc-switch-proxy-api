import { describe, expect, it } from "vitest";
import en from "@/i18n/locales/en.json";
import ja from "@/i18n/locales/ja.json";
import zhTW from "@/i18n/locales/zh-TW.json";
import zh from "@/i18n/locales/zh.json";

type TranslationTree = Record<string, unknown>;

function flattenStrings(
  value: unknown,
  path: string[] = [],
  result = new Map<string, string>(),
): Map<string, string> {
  if (typeof value === "string") {
    result.set(path.join("."), value);
  } else if (typeof value === "object" && value !== null) {
    for (const [key, child] of Object.entries(value)) {
      flattenStrings(child, [...path, key], result);
    }
  }
  return result;
}

function interpolationVariables(value: string): string[] {
  return Array.from(
    value.matchAll(/\{\{\s*([^}]+?)\s*\}\}/g),
    ([, name]) => name,
  ).sort();
}

const reference = flattenStrings(en);
const piKeysOutsideNamespace = new Set([
  "apps.pi",
  "deeplink.api",
  "sessionManager.piDiscoveryUnavailable",
  "sessionManager.piRelativeSessionDir",
  "settings.browsePlaceholderPi",
  "settings.piConfigDir",
  "settings.piConfigDirDescription",
]);
const piReference = new Map(
  [...reference].filter(
    ([key]) => key.startsWith("pi.") || piKeysOutsideNamespace.has(key),
  ),
);
const piProductReferences = new Map(
  [...reference].filter(([, value]) => /\bPi\b/.test(value)),
);
const locales = [
  ["zh", zh],
  ["ja", ja],
  ["zh-TW", zhTW],
] as const;

describe("locale coverage", () => {
  it.each(locales)(
    "covers request trace strings and variables in %s",
    (_name, tree) => {
      const translations = flattenStrings(tree.requestLogs);
      for (const [key, expected] of flattenStrings(en.requestLogs)) {
        const actual = translations.get(key);
        expect(actual, key).toBeDefined();
        expect(interpolationVariables(actual ?? ""), key).toEqual(
          interpolationVariables(expected),
        );
      }
      expect(tree.nav.requestLogs).toBeTruthy();
    },
  );
  it.each(locales)(
    "covers check-in profile keys and variables in %s",
    (_name, tree) => {
      const translations = flattenStrings(tree.checkin as TranslationTree);
      for (const [key, expected] of flattenStrings(
        en.checkin as TranslationTree,
      )) {
        const actual = translations.get(key);
        expect(actual, key).toBeDefined();
        expect(interpolationVariables(actual ?? ""), key).toEqual(
          interpolationVariables(expected),
        );
      }
    },
  );
  it.each([["en", en], ...locales] as const)(
    "explains Cloudflare verification and account Cookie authentication in %s",
    (_name, tree) => {
      expect(tree.checkin.form.browserHint).toContain("Cloudflare");
      expect(tree.checkin.form.browserHint).toContain("Cookie");
    },
  );

  it.each(locales)("covers every Pi translation key in %s", (_name, tree) => {
    const translations = flattenStrings(tree as TranslationTree);
    const missing = [...piReference.keys()].filter(
      (key) => !translations.has(key),
    );

    expect(missing).toEqual([]);
  });

  it.each(locales)(
    "preserves every Pi interpolation variable in %s",
    (_name, tree) => {
      const translations = flattenStrings(tree as TranslationTree);
      const mismatched = [...piReference].flatMap(([key, expected]) => {
        const actual = translations.get(key);
        return actual !== undefined &&
          interpolationVariables(actual).join("\0") !==
            interpolationVariables(expected).join("\0")
          ? [key]
          : [];
      });

      expect(mismatched).toEqual([]);
    },
  );

  it.each(locales)(
    "preserves explicit Pi product mentions in %s",
    (_name, tree) => {
      const translations = flattenStrings(tree as TranslationTree);
      const missingMentions = [...piProductReferences.keys()].filter((key) => {
        const actual = translations.get(key);
        return actual === undefined || !/\bPi\b/.test(actual);
      });

      expect(missingMentions).toEqual([]);
    },
  );
});
