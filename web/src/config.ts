/**
 * The single place the quant API origin is decided. Nothing else may hardcode a
 * host: production builds set `VITE_API_URL`, dev and tests use the default.
 */
export const API_URL = (import.meta.env.VITE_API_URL ?? "http://localhost:8000").replace(/\/+$/, "");

/** Hard ceiling on one API call; the UI shows a timeout error once it passes. */
export const REQUEST_TIMEOUT_MS = 15_000;

/** Trailing debounce for parameter changes (sliders fire on every pixel). */
export const DEBOUNCE_MS = 250;

/** How long a request may sit in "loading" before the wake-up hint appears. */
export const WAKING_MS = 3_000;
