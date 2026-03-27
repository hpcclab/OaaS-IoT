#![allow(unsafe_op_in_unsafe_fn)]

wit_bindgen::generate!({
    world: "oaas-object",
    path: "../../../data-plane/oprc-wasm/wit",
});

use exports::oaas::odgm::guest_object::{
    Guest, InvocationResponse, ObjectProxy,
};
use oaas::odgm::object_context;
use oaas::odgm::types::{FieldEntry, KeyValue, ObjectRef, ResponseStatus};

use serde::{Deserialize, Serialize};

const SIZE: usize = 32;
const WHITE: u32 = 0xFF_FF_FF;

struct GolFunction;

#[derive(Deserialize)]
struct GolInput {
    cols: usize,
    rows: usize,
}

#[derive(Serialize)]
struct GolOutput {
    births: u32,
    deaths: u32,
}

impl Guest for GolFunction {
    fn on_invoke(
        self_proxy: ObjectProxy,
        function_name: String,
        payload: Option<Vec<u8>>,
        _headers: Vec<KeyValue>,
    ) -> InvocationResponse {
        match function_name.as_str() {
            "golStep" => handle_gol_step(self_proxy, payload),
            _ => InvocationResponse {
                status: ResponseStatus::InvalidRequest,
                payload: Some(
                    format!("Unknown function: {function_name}").into_bytes(),
                ),
                headers: vec![],
            },
        }
    }
}

/// Parse a "#RRGGBB" hex color string into a packed u32 (0x00RRGGBB).
fn parse_color(hex: &str) -> u32 {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    u32::from_str_radix(hex, 16).unwrap_or(WHITE)
}

/// Format a packed u32 color back to "#RRGGBB".
fn format_color(c: u32) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (c >> 16) & 0xFF,
        (c >> 8) & 0xFF,
        c & 0xFF
    )
}

/// Average a slice of packed u32 colors.
fn average_colors(colors: &[u32]) -> u32 {
    let n = colors.len() as u32;
    let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
    for &c in colors {
        r += (c >> 16) & 0xFF;
        g += (c >> 8) & 0xFF;
        b += c & 0xFF;
    }
    let r = (r + n / 2) / n;
    let g = (g + n / 2) / n;
    let b = (b + n / 2) / n;
    (r << 16) | (g << 8) | b
}

fn handle_gol_step(
    self_proxy: ObjectProxy,
    payload: Option<Vec<u8>>,
) -> InvocationResponse {
    // Parse input
    let input: GolInput =
        match payload.as_deref().map(serde_json::from_slice).transpose() {
            Ok(Some(v)) => v,
            Ok(None) => {
                return InvocationResponse {
                    status: ResponseStatus::InvalidRequest,
                    payload: Some(
                        b"Missing payload: {\"cols\": N, \"rows\": N}".to_vec(),
                    ),
                    headers: vec![],
                };
            }
            Err(e) => {
                return InvocationResponse {
                    status: ResponseStatus::InvalidRequest,
                    payload: Some(format!("Invalid JSON: {e}").into_bytes()),
                    headers: vec![],
                };
            }
        };

    let cols = input.cols;
    let rows = input.rows;
    let total_w = cols * SIZE;
    let total_h = rows * SIZE;

    // Get identity from self to derive sibling refs (same cls & partition)
    let self_ref = self_proxy.ref_();
    let cls = &self_ref.cls;
    let partition_id = self_ref.partition_id;

    // 1. Fetch all tile proxies and their data
    let mut proxies = Vec::with_capacity(cols * rows);
    let mut all_entries = Vec::with_capacity(cols * rows);

    for cy in 0..rows {
        for cx in 0..cols {
            let obj_ref = ObjectRef {
                cls: cls.clone(),
                partition_id,
                object_id: format!("canvas-{cx}-{cy}"),
            };
            let proxy = match object_context::object(&obj_ref) {
                Ok(p) => p,
                Err(e) => {
                    return InvocationResponse {
                        status: ResponseStatus::SystemError,
                        payload: Some(format!("Failed to get proxy for canvas-{cx}-{cy}: {e:?}").into_bytes()),
                        headers: vec![],
                    };
                }
            };
            let data = match proxy.get_all() {
                Ok(d) => d,
                Err(e) => {
                    return InvocationResponse {
                        status: ResponseStatus::SystemError,
                        payload: Some(
                            format!(
                                "Failed to get data for canvas-{cx}-{cy}: {e:?}"
                            )
                            .into_bytes(),
                        ),
                        headers: vec![],
                    };
                }
            };
            proxies.push(proxy);
            all_entries.push(data);
        }
    }

    // 2. Build global grid from fetched data. WHITE = dead/empty.
    let grid_size = total_w * total_h;
    let mut grid = vec![WHITE; grid_size];

    for (i, data) in all_entries.iter().enumerate() {
        let cx = i % cols;
        let cy = i / cols;
        for entry in &data.entries {
            // Skip metadata keys
            if entry.key.starts_with('_') {
                continue;
            }
            let Some((lx_str, ly_str)) = entry.key.split_once(':') else {
                continue;
            };
            let (Ok(lx), Ok(ly)) =
                (lx_str.parse::<usize>(), ly_str.parse::<usize>())
            else {
                continue;
            };
            if lx >= SIZE || ly >= SIZE {
                continue;
            }
            // Entry value is JSON-encoded string bytes (e.g. "\"#FF0000\"")
            // Try parsing as JSON string first, fall back to raw UTF-8
            let color_str = if let Ok(s) =
                serde_json::from_slice::<String>(&entry.value.data)
            {
                s
            } else if let Ok(s) = core::str::from_utf8(&entry.value.data) {
                s.to_string()
            } else {
                continue;
            };
            let color = parse_color(&color_str);
            if color != WHITE {
                let gx = cx * SIZE + lx;
                let gy = cy * SIZE + ly;
                grid[gy * total_w + gx] = color;
            }
        }
    }

    // 3. Compute next generation
    let mut next_grid = vec![WHITE; grid_size];
    let mut births: u32 = 0;
    let mut deaths: u32 = 0;
    let mut neighbors_buf = Vec::with_capacity(8);

    for gy in 0..total_h {
        for gx in 0..total_w {
            neighbors_buf.clear();
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    if dx == 0 && dy == 0 {
                        continue;
                    }
                    let nx = gx as i32 + dx;
                    let ny = gy as i32 + dy;
                    if nx < 0
                        || nx >= total_w as i32
                        || ny < 0
                        || ny >= total_h as i32
                    {
                        continue;
                    }
                    let c = grid[ny as usize * total_w + nx as usize];
                    if c != WHITE {
                        neighbors_buf.push(c);
                    }
                }
            }

            let idx = gy * total_w + gx;
            let alive = grid[idx] != WHITE;
            let count = neighbors_buf.len();

            if alive && (count == 2 || count == 3) {
                next_grid[idx] = grid[idx]; // survives
            } else if !alive && count == 3 {
                next_grid[idx] = average_colors(&neighbors_buf); // born
                births += 1;
            } else if alive {
                deaths += 1; // dies
            }
        }
    }

    // 4. Write back only changed pixels per tile
    for (i, proxy) in proxies.iter().enumerate() {
        let cx = i % cols;
        let cy = i / cols;
        let mut updates: Vec<FieldEntry> = Vec::new();
        for ly in 0..SIZE {
            for lx in 0..SIZE {
                let gx = cx * SIZE + lx;
                let gy = cy * SIZE + ly;
                let idx = gy * total_w + gx;
                if grid[idx] != next_grid[idx] {
                    let value_str = format_color(next_grid[idx]);
                    // Encode as JSON string to match the TypeScript SDK convention
                    let json_bytes = serde_json::to_vec(&value_str)
                        .unwrap_or_else(|_| value_str.into_bytes());
                    updates.push(FieldEntry {
                        key: format!("{lx}:{ly}"),
                        value: json_bytes,
                    });
                }
            }
        }
        if !updates.is_empty() {
            if let Err(e) = proxy.set_many(&updates) {
                return InvocationResponse {
                    status: ResponseStatus::SystemError,
                    payload: Some(
                        format!("Failed to write tile {cx}-{cy}: {e:?}")
                            .into_bytes(),
                    ),
                    headers: vec![],
                };
            }
        }
    }

    // 5. Return result
    let output = GolOutput { births, deaths };
    let payload = serde_json::to_vec(&output).unwrap_or_default();
    InvocationResponse {
        status: ResponseStatus::Okay,
        payload: Some(payload),
        headers: vec![KeyValue {
            key: "content-type".to_string(),
            value: "application/json".to_string(),
        }],
    }
}

export!(GolFunction);
