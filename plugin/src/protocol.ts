// Wire types for the Host Link Protocol. Mirrors crates/hostlink/src/protocol.rs.
// See PROTOCOL.md §3 — changes land there first.

export const PROTOCOL_VERSION = 1;

export interface Request {
  jsonrpc: "2.0";
  id: number;
  method: string;
  params?: unknown;
}

export interface Response {
  jsonrpc: "2.0";
  id: number;
  result?: unknown;
  error?: ErrorObject;
}

export interface ErrorObject {
  code: number;
  message: string;
}

export interface ProgressNotification {
  jsonrpc: "2.0";
  method: "$/progress";
  params: { id: number; pct?: number; note?: string };
}

export const codes = {
  METHOD_NOT_FOUND: -32601,
  INVALID_PARAMS: -32602,
  HOST_ERROR: -32000,
  NOT_FOUND: -32004,
} as const;

export function ok(id: number, result: unknown): Response {
  return { jsonrpc: "2.0", id, result: result === undefined ? null : result };
}

export function err(id: number, code: number, message: string): Response {
  return { jsonrpc: "2.0", id, error: { code, message } };
}

export function progress(id: number, pct: number, note?: string): ProgressNotification {
  return { jsonrpc: "2.0", method: "$/progress", params: { id, pct, note } };
}

// --- messages across the postMessage boundary (main thread <-> UI iframe) ---

export type ToMain = { kind: "request"; frame: Request } | { kind: "ui-ready" };

export type ToUi =
  | { kind: "reply"; frame: Response | ProgressNotification }
  | { kind: "status"; fileName: string; pageName: string; selectionCount: number };
