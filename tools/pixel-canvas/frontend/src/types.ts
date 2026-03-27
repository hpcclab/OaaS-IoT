/** Shared type definitions for the pixel-canvas frontend. */

/** Canvas dimensions (pixels per side). */
export const CANVAS_SIZE = 32;

/** Default display pixels per canvas pixel (audience mode). */
export const CELL_PX = 10;

/** OaaS class name for pixel canvas objects.
 * Fully-qualified as "{package}.{class}" to match the production PM flow. */
export const CLASS_NAME = "pixel-canvas.pixel-canvas";

/** Default partition. */
export const PARTITION = 0;

/** Map of "x:y" → CSS color string. */
export type PixelMap = Map<string, string>;

/** Optional class-level API overrides. */
export interface ClassConfig {
  /** Full class API base URL, e.g. "http://gw:8080/api/class/my-canvas".
   *  When set, replaces the default "<gateway>/api/class/pixel-canvas" prefix. */
  classBase?: string;
  /** Partition number. Defaults to 0. */
  partition?: number;
}

/** Configuration resolved from URL parameters or config form. */
export interface AppConfig {
  mode: "audience" | "presenter";
  gateway: string;
  /** Optional class API base URL override (see ClassConfig.classBase). */
  classBase?: string;
  /** Optional partition override (see ClassConfig.partition). */
  partition?: number;
  /** Audience: grid column index. */
  gridX?: number;
  /** Audience: grid row index. */
  gridY?: number;
  /** Presenter: number of columns. */
  cols?: number;
  /** Presenter: number of rows. */
  rows?: number;
}

/** A single entry from the gateway object response. */
export interface EntryData {
  data: string;
  type?: string;
}

/** Gateway GET object response shape. */
export interface ObjectResponse {
  metadata?: {
    cls_id?: string;
    partition_id?: number;
    object_id?: string;
  };
  entries?: Record<string, EntryData>;
}
