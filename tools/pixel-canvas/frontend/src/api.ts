/**
 * Gateway API client for pixel-canvas objects.
 *
 * Handles encoding/decoding of pixel data and REST calls to the OaaS gateway.
 */

import { CLASS_NAME, PARTITION } from "./types.js";
import type { PixelMap } from "./types.js";

/** Encode a CSS color string to base64 (UTF-8 safe). */
function encodeColor(color: string): string {
  return btoa(unescape(encodeURIComponent(color)));
}

/** Build the object URL for a canvas tile. */
function objectUrl(gatewayBase: string, gridX: number, gridY: number): string {
  return `${gatewayBase}/api/class/${CLASS_NAME}/${PARTITION}/objects/canvas-${gridX}-${gridY}`;
}

/** Convert an http(s) base URL to a ws(s) URL for WebSocket connections. */
function toWsUrl(httpBase: string): string {
  return httpBase.replace(/^http(s?):\/\//, "ws$1://");
}

export interface FetchResult {
  ok: boolean;
  pixels: PixelMap;
}

/** JSON shape of a single entry returned by the gateway GET object API. */
interface RawEntry {
  data: string; // standard base64-encoded bytes (JSON-serialized value from WASM SDK)
  type: number;
}

/** JSON shape of the WebSocket event payload published by ODGM. */
export interface WsEvent {
  object_id: string;
  cls_id: string;
  partition_id: number;
  source: string;
  /** Object version after this mutation — used for gap detection. */
  version?: number;
  changes: WsChange[];
}

export interface WsChange {
  key: string;
  action: string;
  /** Base64-encoded entry value (present when ws_event_include_values is enabled). */
  value?: string;
}

/**
 * Decode a WsChange value (base64 → JSON-parsed string) into a pixel color.
 * Returns null if no value is present or the payload is not a plain string.
 */
export function decodeChangeValue(change: WsChange): string | null {
  if (!change.value) return null;
  try {
    const bytes = atob(change.value);
    const parsed = JSON.parse(bytes);
    if (typeof parsed === "string") return parsed;
  } catch {
    // ignore
  }
  return null;
}

/**
 * Decode a single raw entry's bytes into a pixel color string.
 * The WASM SDK serializes values as JSON, so bytes contain e.g. `"#FF0000"` (with quotes).
 * Returns the decoded color string, or null if the entry is not a plain string.
 */
function decodeEntryColor(raw: RawEntry): string | null {
  try {
    const bytes = atob(raw.data);
    const value = JSON.parse(bytes);
    if (typeof value === "string") return value;
  } catch {
    // Ignore unparseable entries
  }
  return null;
}

/**
 * Fetch a canvas object via the gateway GET object API (no WASM invocation).
 *
 * GET /api/class/{cls}/{pid}/objects/{oid} with Accept: application/json returns
 * the raw ObjData as JSON.  Entry data bytes are JSON-serialized by the WASM SDK,
 * so we base64-decode and JSON-parse each entry to recover the CSS color string.
 *
 * Returns { ok, pixels } — ok=false on network/server errors.
 * Returns { ok: true, pixels: empty } if the object has no painted pixels.
 */
export async function fetchCanvas(
  gatewayBase: string,
  gridX: number,
  gridY: number
): Promise<FetchResult> {
  const url = objectUrl(gatewayBase, gridX, gridY);
  let res: Response;
  try {
    res = await fetch(url, {
      headers: { Accept: "application/json" },
    });
  } catch (e) {
    console.warn("fetchCanvas network error:", e);
    return { ok: false, pixels: new Map() };
  }
  if (res.status === 404) {
    // Object doesn't exist yet — create it so WASM functions can operate on it
    await saveCanvas(gatewayBase, gridX, gridY, new Map());
    return { ok: true, pixels: new Map() };
  }
  if (!res.ok) {
    console.warn("fetchCanvas error", res.status);
    return { ok: false, pixels: new Map() };
  }

  const obj: { entries?: Record<string, RawEntry> } = await res.json();
  const pixels: PixelMap = new Map();
  for (const [key, entry] of Object.entries(obj.entries ?? {})) {
    if (key.startsWith("_")) continue; // skip internal metadata keys
    const color = decodeEntryColor(entry);
    if (color !== null) pixels.set(key, color);
  }
  return { ok: true, pixels };
}

/**
 * Subscribe to WebSocket events for a single canvas object.
 *
 * Calls `onEvent` for each incoming event and automatically reconnects on
 * disconnect (with a 2-second back-off).  Returns a `destroy()` function
 * that permanently cancels the subscription.
 */
export function subscribeToObject(
  gatewayBase: string,
  gridX: number,
  gridY: number,
  onEvent: (evt: WsEvent) => void
): { destroy: () => void } {
  const wsBase = toWsUrl(gatewayBase);
  const path = `/api/class/${CLASS_NAME}/${PARTITION}/objects/canvas-${gridX}-${gridY}/ws`;
  return connectWs(`${wsBase}${path}`, onEvent);
}

/**
 * Subscribe to WebSocket events for an entire partition (all canvas tiles).
 *
 * The `onEvent` callback receives events for any object in the partition;
 * use `evt.object_id` to determine which tile changed.
 */
export function subscribeToPartition(
  gatewayBase: string,
  onEvent: (evt: WsEvent) => void
): { destroy: () => void } {
  const wsBase = toWsUrl(gatewayBase);
  const path = `/api/class/${CLASS_NAME}/${PARTITION}/ws`;
  return connectWs(`${wsBase}${path}`, onEvent);
}

/** Internal: open a WebSocket, dispatch parsed events, auto-reconnect on close. */
function connectWs(
  url: string,
  onEvent: (evt: WsEvent) => void
): { destroy: () => void } {
  let ws: WebSocket | null = null;
  let destroyed = false;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

  function open() {
    if (destroyed) return;
    ws = new WebSocket(url);
    ws.onmessage = (e) => {
      try {
        const evt: WsEvent = JSON.parse(e.data as string);
        onEvent(evt);
      } catch {
        // Ignore malformed frames
      }
    };
    ws.onclose = () => {
      if (!destroyed) {
        reconnectTimer = setTimeout(open, 2000);
      }
    };
    ws.onerror = () => {
      ws?.close();
    };
  }

  open();

  return {
    destroy() {
      destroyed = true;
      if (reconnectTimer !== null) clearTimeout(reconnectTimer);
      ws?.close();
    },
  };
}

/**
 * Save a full pixel map to the gateway via PUT.
 * Returns true on success, false on error.
 */
export async function saveCanvas(
  gatewayBase: string,
  gridX: number,
  gridY: number,
  pixelMap: PixelMap
): Promise<boolean> {
  const entries: Record<string, { data: string; type: string }> = {};
  for (const [key, color] of pixelMap) {
    entries[key] = { data: encodeColor(color), type: "BYTE" };
  }
  const body = {
    metadata: {
      cls_id: CLASS_NAME,
      partition_id: PARTITION,
      object_id: `canvas-${gridX}-${gridY}`,
    },
    entries,
  };
  const url = objectUrl(gatewayBase, gridX, gridY);
  try {
    const res = await fetch(url, {
      method: "PUT",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      console.warn("saveCanvas PUT failed:", res.status);
      return false;
    }
    return true;
  } catch (e) {
    console.warn("saveCanvas network error:", e);
    return false;
  }
}

/**
 * Invoke paintBatch on a canvas object, sending only the changed pixels.
 * Returns true on success, false on error.
 */
export async function paintBatch(
  gatewayBase: string,
  gridX: number,
  gridY: number,
  pixels: PixelMap
): Promise<boolean> {
  if (pixels.size === 0) return true;
  const entries: Record<string, string> = {};
  for (const [key, color] of pixels) {
    entries[key] = color;
  }
  const url = `${objectUrl(gatewayBase, gridX, gridY)}/invokes/paintBatch`;
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(entries),
    });
    if (!res.ok) {
      console.warn("paintBatch invoke failed:", res.status);
      return false;
    }
    return true;
  } catch (e) {
    console.warn("paintBatch network error:", e);
    return false;
  }
}

export interface GolStepResult {
  births: number;
  deaths: number;
}

/**
 * Invoke one Game of Life step via the gateway.
 * Calls the stateless golStep function on canvas-0-0 (arbitrary; it reads all canvases).
 */
export async function invokeGolStep(
  gatewayBase: string,
  cols: number,
  rows: number,
): Promise<GolStepResult | null> {
  const url = `${gatewayBase}/api/class/${CLASS_NAME}/${PARTITION}/objects/canvas-0-0/invokes/golStep`;
  try {
    const res = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ cols, rows }),
    });
    if (!res.ok) {
      console.warn("golStep invoke failed:", res.status);
      return null;
    }
    return (await res.json()) as GolStepResult;
  } catch (e) {
    console.warn("golStep network error:", e);
    return null;
  }
}
