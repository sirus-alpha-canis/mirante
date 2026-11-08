import { afterEach, describe, expect, it } from "vitest";
import { DICTS, getLang, LANG_KEY, loadLang, saveLang, setLang, t, tn } from "./i18n";

const store = (init: Record<string, string> = {}) => {
  const m = new Map(Object.entries(init));
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
};

afterEach(() => setLang("en"));

describe("i18n", () => {
  it("fills parameters and falls back to English for a missing translation", () => {
    expect(t("next.position", { at: 1, n: 3 })).toBe("1 of 3");
    setLang("pt");
    expect(getLang()).toBe("pt");
    expect(t("next.position", { at: 1, n: 3 })).toBe("1 de 3");
    const pt = DICTS.pt as Record<string, string>;
    const saved = pt["nav.home"];
    delete pt["nav.home"];
    try {
      expect(t("nav.home")).toBe("Home");
    } finally {
      pt["nav.home"] = saved;
    }
  });

  it("picks one or many", () => {
    expect(tn(1, "nudge.one", "nudge.many")).toBe("1 agent has waited on you for over 10 min");
    expect(tn(3, "nudge.one", "nudge.many")).toBe("3 agents have waited on you for over 10 min");
  });

  it("Portuguese translates every key, with the same parameters", () => {
    const en = DICTS.en as Record<string, string>;
    const pt = DICTS.pt as Record<string, string>;
    const params = (s: string) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort();
    expect(Object.keys(en).filter((k) => !(k in pt))).toEqual([]);
    expect(Object.keys(pt).filter((k) => !(k in en))).toEqual([]);
    for (const k of Object.keys(en)) expect(params(pt[k]), k).toEqual(params(en[k]));
  });

  it("product terms keep their agreed Portuguese", () => {
    setLang("pt");
    expect([t("column.needs_you"), t("column.review"), t("column.working"), t("column.idle")]).toEqual([
      "Precisa de você",
      "Pronto para revisar",
      "Trabalhando",
      "Parados",
    ]);
  });

  it("remembers the choice; else follows the system language; storage that throws is ignored", () => {
    expect(loadLang(store(), "pt-BR")).toBe("pt");
    expect(loadLang(store(), "en-US")).toBe("en");
    expect(loadLang(store(), undefined)).toBe("en");
    expect(loadLang(store({ [LANG_KEY]: "en" }), "pt-BR")).toBe("en");
    const s = store();
    saveLang(s, "pt");
    expect(s.m.get(LANG_KEY)).toBe("pt");
    const boom = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    };
    expect(loadLang(boom, "pt-PT")).toBe("pt");
    expect(() => saveLang(boom, "en")).not.toThrow();
  });
});
