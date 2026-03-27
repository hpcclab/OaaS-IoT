/**
 * Render a pixel map onto a canvas element.
 */
import type { PixelMap } from "./types.js";

export function renderPixels(
  canvasEl: HTMLCanvasElement,
  pixelMap: PixelMap,
  cellSize: number
): void {
  const ctx = canvasEl.getContext("2d")!;
  // White background — missing pixels and #FFFFFF both appear as white.
  ctx.fillStyle = "#FFFFFF";
  ctx.fillRect(0, 0, canvasEl.width, canvasEl.height);
  for (const [key, color] of pixelMap) {
    const upper = color.toUpperCase();
    if (upper === "#FFFFFF") continue; // white == background, skip
    const [px, py] = key.split(":").map(Number);
    ctx.fillStyle = color;
    ctx.fillRect(px * cellSize, py * cellSize, cellSize, cellSize);
  }
}
