/**
 * PresenterMosaic — N×M tiled mosaic view for presenter / cloud.
 *
 * Renders a grid of canvas tiles, auto-refreshes via polling,
 * and supports click-to-edit on individual tiles.
 */

import { CANVAS_SIZE } from "./types.js";
import type { PixelMap, ClassConfig } from "./types.js";
import { fetchCanvas, paintBatch, invokeGolStep, subscribeToPartition, decodeChangeValue } from "./api.js";
import type { FetchResult, WsEvent } from "./api.js";
import { renderPixels } from "./render.js";

export class PresenterMosaic {
  private readonly gatewayBase: string;
  private readonly cols: number;
  private readonly rows: number;
  private readonly cellSize: number;
  private readonly opts: ClassConfig;

  /** pixelMaps[x][y] = Map<"px:py", color> */
  private readonly pixelMaps: PixelMap[][];

  private wsSub: { destroy: () => void } | null = null;
  private canvases: { el: HTMLCanvasElement; x: number; y: number }[] = [];

  private statusEl!: HTMLSpanElement;

  /** Direct-draw state */
  private currentColor = "#000000";
  private readonly dirtyMaps = new Map<string, Set<string>>();
  private readonly flushTimers = new Map<string, ReturnType<typeof setTimeout>>();

  /** Per-tile last received object version — key: "x:y". Used for WS gap detection. */
  private readonly tileVersions = new Map<string, number>();

  /** GoL auto-run state */
  private golRunning = false;
  private golTimer: ReturnType<typeof setTimeout> | null = null;
  private golGeneration = 0;
  private golBusy = false;

  constructor(
    container: HTMLElement,
    gatewayBase: string,
    cols: number,
    rows: number,
    opts: ClassConfig = {}
  ) {
    this.gatewayBase = gatewayBase;
    this.cols = cols;
    this.rows = rows;
    this.opts = opts;

    // Compute tile display size so mosaic fits in ~700px
    this.cellSize = Math.max(
      1,
      Math.floor(Math.min(700 / cols, 700 / rows) / CANVAS_SIZE)
    );

    this.pixelMaps = Array.from({ length: cols }, () =>
      Array.from({ length: rows }, () => new Map<string, string>())
    );

    this.buildUI(container);
    this.fetchAll();
    this.startWebSocket();
  }

  private buildUI(container: HTMLElement): void {
    const tilePx = this.cellSize * CANVAS_SIZE;

    container.innerHTML = `
      <div class="presenter-mosaic">
        <div class="presenter-toolbar">
          <span class="js-mosaic-status status-indicator">● loading...</span>
          <span class="mosaic-info">${this.cols}×${this.rows} | ${this.gatewayBase}</span>
          <label class="color-label">Color <input type="color" class="js-draw-color" value="#000000"></label>
        </div>
        <div class="gol-controls">
          <button class="btn-small js-gol-step" title="Run one Game of Life step">⏩ Step</button>
          <button class="btn-small js-gol-toggle" title="Start/stop auto-run">▶ Run</button>
          <label class="gol-speed-label">
            Speed
            <input type="range" class="js-gol-speed" min="100" max="3000" value="500" step="100">
            <span class="js-gol-speed-val">500ms</span>
          </label>
          <span class="js-gol-info gol-info"></span>
        </div>
        <div class="js-mosaic-grid mosaic-grid"
          style="grid-template-columns:repeat(${this.cols}, ${tilePx}px)">
        </div>
      </div>`;

    this.statusEl = container.querySelector(".js-mosaic-status")!;
    const gridEl = container.querySelector(".js-mosaic-grid")!;

    const colorPicker = container.querySelector(".js-draw-color") as HTMLInputElement;
    colorPicker.addEventListener("input", () => { this.currentColor = colorPicker.value; });

    // GoL controls wiring
    const stepBtn = container.querySelector(".js-gol-step") as HTMLButtonElement;
    const toggleBtn = container.querySelector(".js-gol-toggle") as HTMLButtonElement;
    const speedSlider = container.querySelector(".js-gol-speed") as HTMLInputElement;
    const speedVal = container.querySelector(".js-gol-speed-val")!;
    const golInfo = container.querySelector(".js-gol-info")!;

    stepBtn.addEventListener("click", () => this.runOneStep(golInfo));
    toggleBtn.addEventListener("click", () => {
      this.golRunning = !this.golRunning;
      toggleBtn.textContent = this.golRunning ? "⏸ Pause" : "▶ Run";
      if (this.golRunning) {
        this.scheduleAutoStep(golInfo, parseInt(speedSlider.value, 10));
      } else {
        this.stopAutoRun();
      }
    });
    speedSlider.addEventListener("input", () => {
      const ms = parseInt(speedSlider.value, 10);
      speedVal.textContent = `${ms}ms`;
    });

    this.canvases = [];
    for (let y = 0; y < this.rows; y++) {
      for (let x = 0; x < this.cols; x++) {
        const el = document.createElement("canvas");
        el.width = tilePx;
        el.height = tilePx;
        el.title = `canvas-${x}-${y}`;
        el.className = "mosaic-tile";
        el.style.cursor = "crosshair";
        this.attachDrawEvents(el, x, y);
        gridEl.appendChild(el);
        this.canvases.push({ el, x, y });
      }
    }
  }

  private canvasEl(x: number, y: number): HTMLCanvasElement | undefined {
    return this.canvases.find((c) => c.x === x && c.y === y)?.el;
  }

  private async fetchAll(): Promise<void> {
    const promises: Promise<{ x: number; y: number; result: FetchResult }>[] = [];
    let anyFailed = false;
    for (let x = 0; x < this.cols; x++) {
      for (let y = 0; y < this.rows; y++) {
        promises.push(
          fetchCanvas(this.gatewayBase, x, y, this.opts).then((result) => ({ x, y, result }))
        );
      }
    }
    const results = await Promise.allSettled(promises);
    for (const r of results) {
      if (r.status !== "fulfilled") { anyFailed = true; continue; }
      const { x, y, result } = r.value;
      if (!result.ok) { anyFailed = true; continue; }
      this.pixelMaps[x][y] = result.pixels;
      const el = this.canvasEl(x, y);
      if (el) renderPixels(el, result.pixels, this.cellSize);
    }
    if (anyFailed) {
      this.statusEl.textContent = `● offline`;
      this.statusEl.style.color = "#ef4444";
    } else {
      this.statusEl.textContent = `● live — ${new Date().toLocaleTimeString()}`;
      this.statusEl.style.color = "#22c55e";
    }
  }

  private startWebSocket(): void {
    this.wsSub = subscribeToPartition(this.gatewayBase, (evt: WsEvent) => {
      // Parse tile coordinates from object_id (format: "canvas-{x}-{y}")
      const m = evt.object_id.match(/^canvas-(\d+)-(\d+)$/);
      if (!m) return;
      const x = parseInt(m[1], 10);
      const y = parseInt(m[2], 10);
      if (x >= this.cols || y >= this.rows) return;

      const vk = `${x}:${y}`;
      const last = this.tileVersions.get(vk);

      // Gap detection: if the server-side version jumped by more than 1 we
      // missed at least one event (server queue overflow under heavy load).
      // Re-fetch this single tile to reconcile, then apply no further delta
      // — the fetch result is already authoritative.
      if (evt.version !== undefined && last !== undefined && evt.version > last + 1) {
        if (evt.version !== undefined) this.tileVersions.set(vk, evt.version);
        fetchCanvas(this.gatewayBase, x, y, this.opts).then((result) => {
          if (!result.ok) return;
          this.pixelMaps[x][y] = result.pixels;
          const el = this.canvasEl(x, y);
          if (el) renderPixels(el, result.pixels, this.cellSize);
        });
        return;
      }

      if (evt.version !== undefined) this.tileVersions.set(vk, evt.version);

      const pixels = this.pixelMaps[x][y];
      for (const change of evt.changes) {
        if (change.key.startsWith("_")) continue;
        if (change.action === "delete") {
          pixels.set(change.key, "#FFFFFF");
          continue;
        }
        const color = decodeChangeValue(change);
        if (color !== null) pixels.set(change.key, color);
      }

      const el = this.canvasEl(x, y);
      if (el) renderPixels(el, pixels, this.cellSize);
      this.statusEl.textContent = `● live — ${new Date().toLocaleTimeString()}`;
      this.statusEl.style.color = "#22c55e";
    }, this.opts);
  }

  /** Attach direct pointer-drawing events to a tile canvas element. */
  private attachDrawEvents(el: HTMLCanvasElement, x: number, y: number): void {
    let isDrawing = false;
    let lastPx: number | null = null;
    let lastPy: number | null = null;

    const paintAt = (e: PointerEvent): void => {
      if (!isDrawing) return;
      const rect = el.getBoundingClientRect();
      const scaleX = el.width / rect.width;
      const scaleY = el.height / rect.height;
      const px = Math.floor((e.clientX - rect.left) * scaleX / this.cellSize);
      const py = Math.floor((e.clientY - rect.top) * scaleY / this.cellSize);
      if (px < 0 || px >= CANVAS_SIZE || py < 0 || py >= CANVAS_SIZE) return;
      if (lastPx !== null && lastPy !== null) {
        this.paintLineOnTile(x, y, lastPx, lastPy, px, py);
      } else {
        this.paintPixelOnTile(x, y, px, py);
      }
      lastPx = px;
      lastPy = py;
      const tileEl = this.canvasEl(x, y);
      if (tileEl) renderPixels(tileEl, this.pixelMaps[x][y], this.cellSize);
      this.scheduleTileFlush(x, y);
    };

    el.addEventListener("pointerdown", (e) => {
      isDrawing = true;
      lastPx = null;
      lastPy = null;
      el.setPointerCapture(e.pointerId);
      paintAt(e);
    });
    el.addEventListener("pointermove", paintAt);
    el.addEventListener("pointerup", () => { isDrawing = false; lastPx = null; lastPy = null; });
    el.addEventListener("pointercancel", () => { isDrawing = false; lastPx = null; lastPy = null; });
  }

  private paintPixelOnTile(x: number, y: number, px: number, py: number): void {
    const key = `${px}:${py}`;
    const pixels = this.pixelMaps[x][y];
    if ((pixels.get(key) ?? "#FFFFFF") === this.currentColor) return;
    pixels.set(key, this.currentColor);
    this.dirtyForTile(x, y).add(key);
  }

  /** Bresenham's line algorithm across a single tile. */
  private paintLineOnTile(x: number, y: number, x0: number, y0: number, x1: number, y1: number): void {
    let dx = Math.abs(x1 - x0), dy = Math.abs(y1 - y0);
    const sx = x0 < x1 ? 1 : -1, sy = y0 < y1 ? 1 : -1;
    let err = dx - dy;
    while (true) {
      this.paintPixelOnTile(x, y, x0, y0);
      if (x0 === x1 && y0 === y1) break;
      const e2 = 2 * err;
      if (e2 > -dy) { err -= dy; x0 += sx; }
      if (e2 < dx)  { err += dx; y0 += sy; }
    }
  }

  private dirtyForTile(x: number, y: number): Set<string> {
    const key = `${x}:${y}`;
    let s = this.dirtyMaps.get(key);
    if (!s) { s = new Set(); this.dirtyMaps.set(key, s); }
    return s;
  }

  private scheduleTileFlush(x: number, y: number): void {
    const key = `${x}:${y}`;
    const existing = this.flushTimers.get(key);
    if (existing !== undefined) clearTimeout(existing);
    this.flushTimers.set(key, setTimeout(() => this.flushTile(x, y), 300));
  }

  private async flushTile(x: number, y: number): Promise<void> {
    const dirty = this.dirtyForTile(x, y);
    if (dirty.size === 0) return;
    const saved = new Set(dirty);
    dirty.clear();
    const batch: PixelMap = new Map();
    for (const k of saved) {
      const color = this.pixelMaps[x][y].get(k);
      if (color !== undefined) batch.set(k, color);
    }
    const ok = await paintBatch(this.gatewayBase, x, y, batch, this.opts);
    if (!ok) {
      for (const k of saved) dirty.add(k);
    }
  }

  /** Run a single GoL step. Canvas updates arrive via WS events; gap detection handles any drops. */
  private async runOneStep(infoEl: Element): Promise<void> {
    if (this.golBusy) return;
    this.golBusy = true;
    infoEl.textContent = "computing...";
    const result = await invokeGolStep(this.gatewayBase, this.cols, this.rows, this.opts);
    if (result) {
      this.golGeneration++;
      infoEl.textContent = `gen ${this.golGeneration} | +${result.births} −${result.deaths}`;
    } else {
      infoEl.textContent = "step failed";
    }
    this.golBusy = false;
  }

  /** Schedule the next auto-run step after delay. */
  private scheduleAutoStep(infoEl: Element, delayMs: number): void {
    if (!this.golRunning) return;
    this.golTimer = setTimeout(async () => {
      await this.runOneStep(infoEl);
      if (this.golRunning) {
        // Re-read slider value each iteration so speed changes take effect
        const slider = document.querySelector(".js-gol-speed") as HTMLInputElement | null;
        const ms = slider ? parseInt(slider.value, 10) : delayMs;
        this.scheduleAutoStep(infoEl, ms);
      }
    }, delayMs);
  }

  /** Stop auto-run. */
  private stopAutoRun(): void {
    if (this.golTimer !== null) {
      clearTimeout(this.golTimer);
      this.golTimer = null;
    }
  }

  destroy(): void {
    this.wsSub?.destroy();
    this.stopAutoRun();
    for (const timer of this.flushTimers.values()) clearTimeout(timer);
  }
}
