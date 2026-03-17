/**
 * AudienceCanvas — 32×32 drawable canvas for audience / edge.
 *
 * Handles pointer/touch drawing, color picker, debounced saves,
 * and periodic polling for remote updates.
 */

import { CANVAS_SIZE, CELL_PX } from "./types.js";
import type { PixelMap } from "./types.js";
import { fetchCanvas, paintBatch, subscribeToObject, decodeChangeValue } from "./api.js";
import type { WsEvent } from "./api.js";
import { renderPixels } from "./render.js";

export class AudienceCanvas {
  private readonly gatewayBase: string;
  private readonly gridX: number;
  private readonly gridY: number;
  private readonly cellSize = CELL_PX;

  private pixels: PixelMap = new Map();
  private dirty = new Set<string>();
  private isDrawing = false;
  private currentColor = "#000000";
  private flushTimer: ReturnType<typeof setTimeout> | null = null;
  private wsSub: { destroy: () => void } | null = null;
  private lastPx: number | null = null;
  private lastPy: number | null = null;

  private canvasEl!: HTMLCanvasElement;
  private colorPicker!: HTMLInputElement;
  private statusEl!: HTMLSpanElement;

  constructor(
    container: HTMLElement,
    gatewayBase: string,
    gridX: number,
    gridY: number
  ) {
    this.gatewayBase = gatewayBase;
    this.gridX = gridX;
    this.gridY = gridY;

    this.buildUI(container);
    this.attachEvents();
    this.fetchAndRender();
    this.startWebSocket();
  }

  private buildUI(container: HTMLElement): void {
    const size = CANVAS_SIZE * this.cellSize;

    container.innerHTML = `
      <div class="audience-canvas">
        <div class="audience-toolbar">
          <label class="color-label">
            Color <input type="color" class="js-color-picker" value="#000000">
          </label>
          <button class="btn-small js-clear-btn">Clear</button>
          <span class="js-status status-indicator">●</span>
        </div>
        <canvas class="js-draw-canvas draw-canvas"
          width="${size}" height="${size}">
        </canvas>
        <span class="canvas-label">canvas-${this.gridX}-${this.gridY}</span>
      </div>`;

    this.canvasEl = container.querySelector(".js-draw-canvas")!;
    this.colorPicker = container.querySelector(".js-color-picker")!;
    this.statusEl = container.querySelector(".js-status")!;
  }

  private attachEvents(): void {
    const canvas = this.canvasEl;

    this.colorPicker.addEventListener("input", (e) => {
      this.currentColor = (e.target as HTMLInputElement).value;
    });

    const clearBtn = this.canvasEl
      .closest(".audience-canvas")!
      .querySelector(".js-clear-btn")!;
    clearBtn.addEventListener("click", () => {
      this.pixels.clear();
      for (let x = 0; x < CANVAS_SIZE; x++)
        for (let y = 0; y < CANVAS_SIZE; y++)
          this.pixels.set(`${x}:${y}`, "#ffffff");
      this.dirty = new Set(this.pixels.keys());
      this.render();
      this.scheduleSave();
    });

    const paint = (e: PointerEvent): void => {
      if (!this.isDrawing) return;
      const rect = canvas.getBoundingClientRect();
      const scaleX = (CANVAS_SIZE * this.cellSize) / rect.width;
      const scaleY = (CANVAS_SIZE * this.cellSize) / rect.height;
      const px = Math.floor(((e.clientX - rect.left) * scaleX) / this.cellSize);
      const py = Math.floor(((e.clientY - rect.top) * scaleY) / this.cellSize);
      if (px < 0 || px >= CANVAS_SIZE || py < 0 || py >= CANVAS_SIZE) return;
      if (this.lastPx !== null && this.lastPy !== null) {
        this.paintLine(this.lastPx, this.lastPy, px, py);
      } else {
        this.paintPixel(px, py);
      }
      this.lastPx = px;
      this.lastPy = py;
      this.render();
      this.scheduleSave();
    };

    canvas.addEventListener("pointerdown", (e) => {
      this.isDrawing = true;
      this.lastPx = null;
      this.lastPy = null;
      canvas.setPointerCapture(e.pointerId);
      paint(e);
    });
    canvas.addEventListener("pointermove", paint);
    canvas.addEventListener("pointerup", () => {
      this.isDrawing = false;
      this.lastPx = null;
      this.lastPy = null;
    });
    canvas.addEventListener("pointercancel", () => {
      this.isDrawing = false;
      this.lastPx = null;
      this.lastPy = null;
    });
  }

  private paintPixel(px: number, py: number): void {
    const key = `${px}:${py}`;
    if (this.pixels.get(key) === this.currentColor) return;
    this.pixels.set(key, this.currentColor);
    this.dirty.add(key);
  }

  /** Bresenham's line algorithm — fills all pixels between (x0,y0) and (x1,y1). */
  private paintLine(x0: number, y0: number, x1: number, y1: number): void {
    let dx = Math.abs(x1 - x0);
    let dy = Math.abs(y1 - y0);
    const sx = x0 < x1 ? 1 : -1;
    const sy = y0 < y1 ? 1 : -1;
    let err = dx - dy;
    while (true) {
      this.paintPixel(x0, y0);
      if (x0 === x1 && y0 === y1) break;
      const e2 = 2 * err;
      if (e2 > -dy) { err -= dy; x0 += sx; }
      if (e2 < dx)  { err += dx; y0 += sy; }
    }
  }

  private render(): void {
    renderPixels(this.canvasEl, this.pixels, this.cellSize);
  }

  private setStatus(ok: boolean, text: string): void {
    this.statusEl.textContent = `● ${text}`;
    this.statusEl.style.color = ok ? "#22c55e" : "#ef4444";
  }

  private scheduleSave(): void {
    if (this.flushTimer !== null) clearTimeout(this.flushTimer);
    this.flushTimer = setTimeout(() => this.flush(), 300);
  }

  private async flush(): Promise<void> {
    if (this.dirty.size === 0) return;
    const savedDirty = new Set(this.dirty);
    // Build a map of only the dirty pixels to send via paintBatch
    const dirtyPixels: Map<string, string> = new Map();
    for (const key of savedDirty) {
      const color = this.pixels.get(key);
      if (color !== undefined) dirtyPixels.set(key, color);
    }
    this.dirty.clear();
    const ok = await paintBatch(this.gatewayBase, this.gridX, this.gridY, dirtyPixels);
    if (ok) {
      this.setStatus(true, "saved");
    } else {
      // Restore dirty keys so they get retried
      for (const key of savedDirty) this.dirty.add(key);
      this.setStatus(false, "save failed");
    }
  }

  private async fetchAndRender(): Promise<void> {
    const result = await fetchCanvas(
      this.gatewayBase,
      this.gridX,
      this.gridY
    );
    if (!result.ok) {
      this.setStatus(false, "offline");
      return;
    }
    // Remote wins only for pixels not currently dirty
    for (const [key, color] of result.pixels) {
      if (!this.dirty.has(key)) {
        this.pixels.set(key, color);
      }
    }
    this.render();
    this.setStatus(true, "synced");
  }

  private startWebSocket(): void {
    this.wsSub = subscribeToObject(
      this.gatewayBase,
      this.gridX,
      this.gridY,
      (evt: WsEvent) => this.handleWsEvent(evt)
    );
  }

  /**
   * Handle an incoming WS event.  If all changes carry inline values we
   * apply the deltas directly (no HTTP round-trip).  Otherwise we fall back
   * to a full fetchAndRender.
   */
  private handleWsEvent(evt: WsEvent): void {
    let allHaveValues = true;
    for (const change of evt.changes) {
      if (change.key.startsWith("_")) continue;
      if (change.action === "delete") {
        // Deletes never carry a value — apply directly
        if (!this.dirty.has(change.key)) {
          this.pixels.delete(change.key);
        }
        continue;
      }
      const color = decodeChangeValue(change);
      if (color !== null) {
        if (!this.dirty.has(change.key)) {
          this.pixels.set(change.key, color);
        }
      } else {
        allHaveValues = false;
      }
    }
    if (allHaveValues) {
      this.render();
      this.setStatus(true, "synced");
    } else {
      this.fetchAndRender();
    }
  }

  destroy(): void {
    if (this.flushTimer !== null) clearTimeout(this.flushTimer);
    this.wsSub?.destroy();
  }
}
