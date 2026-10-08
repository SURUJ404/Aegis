/**
 * Display formatting. One rule above all: a missing, non-finite or absurd value
 * renders as an em dash — never "NaN", "undefined" or a wall of digits.
 */
export const EM_DASH = "—";

const MISSING_TEXT = /^(nan|inf|-inf|infinity|-infinity|undefined|null|none)$/i;

export function isMissing(v: unknown): boolean {
  if (v === null || v === undefined) return true;
  if (typeof v === "number") return !Number.isFinite(v);
  if (typeof v === "string") {
    const s = v.trim();
    return s === "" || MISSING_TEXT.test(s);
  }
  return false;
}

/** 2 decimals by default, grouped, em dash when it cannot be shown. */
export function fmtNum(v: unknown, digits = 2): string {
  if (isMissing(v)) return EM_DASH;
  const n = Number(v);
  return n.toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits });
}

/** A signed fraction as a percentage: `+6.02%`, `-26.34%`. */
export function fmtPct(v: unknown, digits = 2, sign: boolean | "auto" = "auto"): string {
  if (isMissing(v)) return EM_DASH;
  const n = Number(v) * 100;
  if (!Number.isFinite(n)) return EM_DASH;
  const body = Math.abs(n).toLocaleString("en-US", {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  });
  if (sign === false) return `${body}%`;
  const prefix = n > 0 ? "+" : n < 0 ? "-" : "";
  return `${prefix}${body}%`;
}

/** Integer counters (trades, bars, days). */
export function fmtInt(v: unknown): string {
  if (isMissing(v)) return EM_DASH;
  const n = Number(v);
  if (!Number.isInteger(n)) return fmtNum(v, 2);
  return n.toLocaleString("en-US");
}

/** Arbitrary text (symbols, signals, API detail): capped so layout can't blow up. */
export function fmtText(v: unknown, max = 32): string {
  if (isMissing(v)) return EM_DASH;
  const s = String(v);
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

/** Date labels for chart axes — keep them short and never blank. */
export function fmtDate(v: unknown): string {
  if (isMissing(v)) return EM_DASH;
  const s = String(v);
  return s.length > 10 ? s.slice(0, 10) : s;
}
