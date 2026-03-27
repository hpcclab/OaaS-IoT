import { OPackage } from "./bindings/OPackage";
import { OClassDeployment } from "./bindings/OClassDeployment";
import { OObject } from "./types";
import { ClusterInfo } from "./types";

const API_BASE = process.env.NEXT_PUBLIC_API_URL || "";
const API_V1 = `${API_BASE}/api/v1`;

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------
export class ApiError extends Error {
    constructor(public status: number, message: string) {
        super(message);
        this.name = "ApiError";
    }
}

async function throwIfNotOk(res: Response, action: string) {
    if (!res.ok) {
        let msg = `${action} failed: ${res.status}`;
        try {
            const body = await res.json();
            if (body.error) msg = body.error;
        } catch { /* ignore parse errors */ }
        throw new ApiError(res.status, msg);
    }
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------
export interface HealthResponse {
    status: "healthy" | "unhealthy";
    service: string;
    version: string;
    timestamp: string;
    storage: { status: string; message?: string };
}

export async function fetchHealth(): Promise<HealthResponse> {
    const res = await fetch(`${API_BASE}/health`);
    return await res.json();
}

// ---------------------------------------------------------------------------
// Topology
// ---------------------------------------------------------------------------
export interface TopologySnapshot {
    nodes: Array<{
        id: string;
        node_type: string;
        status?: string;
        metadata?: Record<string, string>;
        deployed_classes?: string[];
    }>;
    edges: Array<{
        from_id: string;
        to_id: string;
        metadata?: Record<string, string>;
    }>;
    timestamp?: { seconds: number; nanos: number };
}

export async function fetchTopology(source: "deployments" | "zenoh" = "deployments"): Promise<TopologySnapshot> {
    const res = await fetch(`${API_V1}/topology?source=${source}`);
    if (!res.ok) throw new ApiError(res.status, `Failed to fetch topology: ${res.status}`);
    return await res.json();
}

export async function fetchPackages(): Promise<OPackage[]> {
    try {
        const res = await fetch(`${API_V1}/packages`);
        if (!res.ok) throw new Error(`Failed to fetch packages: ${res.status}`);
        return await res.json();
    } catch (e) {
        console.error(e);
        return [];
    }
}

export async function fetchPackage(name: string): Promise<OPackage> {
    const res = await fetch(`${API_V1}/packages/${encodeURIComponent(name)}`);
    await throwIfNotOk(res, "Fetch package");
    return await res.json();
}

export async function createPackage(pkg: OPackage): Promise<{ id: string; status: string; message?: string }> {
    const res = await fetch(`${API_V1}/packages`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(pkg),
    });
    await throwIfNotOk(res, "Create package");
    return await res.json();
}

export async function deletePackage(name: string): Promise<void> {
    const res = await fetch(`${API_V1}/packages/${encodeURIComponent(name)}`, {
        method: "DELETE",
    });
    await throwIfNotOk(res, "Delete package");
}

export async function fetchDeployments(): Promise<OClassDeployment[]> {
    try {
        const res = await fetch(`${API_V1}/deployments`);
        if (!res.ok) throw new Error(`Failed to fetch deployments: ${res.status}`);
        return await res.json();
    } catch (e) {
        console.error(e);
        return [];
    }
}

export async function createDeployment(dep: Partial<OClassDeployment>): Promise<{ id: string; status: string; message?: string }> {
    const res = await fetch(`${API_V1}/deployments`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(dep),
    });
    await throwIfNotOk(res, "Create deployment");
    return await res.json();
}

export async function deleteDeployment(key: string): Promise<{ message: string; id: string; deleted_envs: string[] }> {
    const res = await fetch(`${API_V1}/deployments/${encodeURIComponent(key)}`, {
        method: "DELETE",
    });
    await throwIfNotOk(res, "Delete deployment");
    return await res.json();
}

export async function fetchEnvironments(): Promise<ClusterInfo[]> {
    try {
        const res = await fetch(`${API_V1}/envs`);
        if (!res.ok) throw new Error(`Failed to fetch environments: ${res.status}`);
        const data = await res.json();

        return data.map((env: any) => {
            const h = env.health || {};
            return {
                name: typeof env === 'string' ? env : (env.name || "Unknown"),
                status: h.status || "Healthy",
                crmVersion: h.crm_version,
                nodes: typeof h.ready_nodes === 'number' && typeof h.node_count === 'number'
                    ? `${h.ready_nodes}/${h.node_count} ready`
                    : undefined,
                avail: typeof h.availability === 'number'
                    ? `${(h.availability * 100).toFixed(1)}%`
                    : undefined,
                lastSeen: h.last_seen ? new Date(h.last_seen).toLocaleString() : undefined,
                gateway_url: env.gateway_url,
                raw: env,
            };
        });
    } catch (e) {
        console.error(e);
        return [];
    }
}

// ---------------------------------------------------------------------------
// Gateway helpers
// ---------------------------------------------------------------------------

/** Build the gateway base URL, optionally scoped to an environment.
 *  When a `gatewayUrl` is provided (from the envs API), use it directly.
 *  Otherwise fall back to path-based routing on the same origin. */
function gatewayBase(env?: string, gatewayUrl?: string): string {
    if (gatewayUrl) return gatewayUrl;
    return env
        ? `${API_BASE}/api/gateway/env/${encodeURIComponent(env)}`
        : `${API_BASE}/api/gateway`;
}

export async function fetchObjects(classKey: string, partition: number, env?: string, gatewayUrl?: string): Promise<OObject[]> {
    try {
        const res = await fetch(`${gatewayBase(env, gatewayUrl)}/api/class/${classKey}/${partition}/objects`);
        if (!res.ok) throw new Error(`Failed to fetch objects: ${res.status}`);
        const data = await res.json();
        // Gateway returns {objects: [{object_id, version, entry_count}]}
        // Map to frontend OObject shape ({id, version, entry_count})
        const rawObjects: unknown[] = data.objects || [];
        return rawObjects.map((o: any) => ({
            id: o.object_id ?? o.id ?? "",
            version: o.version,
            entry_count: o.entry_count,
        }));
    } catch (e) {
        console.error(e);
        return [];
    }
}

/**
 * Fetch a single object from the gateway.
 * Gateway returns ObjData (protobuf/JSON):
 *   { metadata: { object_id, cls_id, partition_id }, entries: { key: { data: base64, type: 0 } }, event: ... }
 */
export async function fetchObject(classKey: string, partition: number, objectId: string, env?: string, gatewayUrl?: string): Promise<unknown> {
    const res = await fetch(
        `${gatewayBase(env, gatewayUrl)}/api/class/${classKey}/${partition}/objects/${encodeURIComponent(objectId)}`,
        { headers: { "Accept": "application/json" } }
    );
    await throwIfNotOk(res, "Fetch object");
    return await res.json();
}

/**
 * Encode a plain JSON entries map into the ObjData format expected by the gateway.
 * Each entry value is JSON-serialized → UTF-8 bytes → base64.
 */
export function encodeEntries(entries: Record<string, unknown>): Record<string, { data: string; type: number }> {
    const encoded: Record<string, { data: string; type: number }> = {};
    for (const [key, value] of Object.entries(entries)) {
        const jsonStr = JSON.stringify(value);
        encoded[key] = { data: btoa(jsonStr), type: 0 };
    }
    return encoded;
}

/**
 * Create or update an object via PUT.
 * `objData` should be in the gateway's ObjData format:
 *   { entries: { key: { data: base64, type: 0 } } }
 */
export async function createOrUpdateObject(
    classKey: string,
    partition: number,
    objectId: string,
    objData: unknown,
    env?: string,
    gatewayUrl?: string,
): Promise<void> {
    const res = await fetch(
        `${gatewayBase(env, gatewayUrl)}/api/class/${classKey}/${partition}/objects/${encodeURIComponent(objectId)}`,
        {
            method: "PUT",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(objData),
        }
    );
    await throwIfNotOk(res, "Create/update object");
}

export async function deleteObject(classKey: string, partition: number, objectId: string, env?: string, gatewayUrl?: string): Promise<void> {
    const res = await fetch(
        `${gatewayBase(env, gatewayUrl)}/api/class/${classKey}/${partition}/objects/${encodeURIComponent(objectId)}`,
        { method: "DELETE" }
    );
    await throwIfNotOk(res, "Delete object");
}

export async function invokeFunction(
    classKey: string,
    partition: number,
    objectId: string,
    functionName: string,
    payload: unknown,
    env?: string,
    gatewayUrl?: string,
): Promise<unknown> {
    try {
        const res = await fetch(`${gatewayBase(env, gatewayUrl)}/api/class/${classKey}/${partition}/objects/${encodeURIComponent(objectId)}/invokes/${encodeURIComponent(functionName)}`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify(payload),
        });
        if (!res.ok) {
            const text = await res.text();
            throw new Error(`Invocation failed: ${res.status} ${text}`);
        }
        return await res.json();
    } catch (e) {
        console.error(e);
        throw e;
    }
}

// Helper to use with SWR
export const fetcher = (url: string) => {
    // Check if url starts with /, prepend API_BASE if needed, but SWR might use full URL
    // If we use relative URLs in SWR keys, we need to handle it.
    // For now, assume keys are relative to API_V1 or API_BASE?
    // Let's make fetcher robust.
    const fullUrl = url.startsWith("http") ? url : `${API_BASE}${url}`;
    return fetch(fullUrl).then((res) => res.json());
};

// ---------------------------------------------------------------------------
// Debug – Network partition simulation (pairwise connectivity model)
// ---------------------------------------------------------------------------

export interface LinkState {
    env_a: string;
    env_b: string;
    connected: boolean;
    latency_ms: number;
}

export interface NetworkOverview {
    environments: string[];
    links: LinkState[];
}

export interface LinkActionResponse {
    env_a: string;
    env_b: string;
    action: string;
    connected: boolean;
    latency_ms: number;
}

export interface EnvActionResponse {
    env: string;
    action: string;
    affected_links: string[];
}

export interface BulkActionResponse {
    action: string;
    environments: string[];
}

export async function fetchNetworkState(): Promise<NetworkOverview> {
    const res = await fetch(`${API_V1}/network-sim`);
    await throwIfNotOk(res, "Fetch network state");
    return await res.json();
}

/** Partition a single link between two environments. */
export async function partitionLink(envA: string, envB: string, latencyMs?: number): Promise<LinkActionResponse> {
    const res = await fetch(`${API_V1}/network-sim/${encodeURIComponent(envA)}/${encodeURIComponent(envB)}/partition`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ latency_ms: latencyMs ?? null }),
    });
    await throwIfNotOk(res, "Partition link");
    return await res.json();
}

/** Set latency on a link without changing connectivity. */
export async function setLinkLatency(envA: string, envB: string, latencyMs: number): Promise<LinkActionResponse> {
    const res = await fetch(`${API_V1}/network-sim/${encodeURIComponent(envA)}/${encodeURIComponent(envB)}/latency`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ latency_ms: latencyMs }),
    });
    await throwIfNotOk(res, "Set link latency");
    return await res.json();
}

/** Heal a single link between two environments. */
export async function healLink(envA: string, envB: string): Promise<LinkActionResponse> {
    const res = await fetch(`${API_V1}/network-sim/${encodeURIComponent(envA)}/${encodeURIComponent(envB)}/heal`, {
        method: "POST",
    });
    await throwIfNotOk(res, "Heal link");
    return await res.json();
}

/** Partition an environment from ALL others. */
export async function partitionEnv(env: string, latencyMs?: number): Promise<EnvActionResponse> {
    const res = await fetch(`${API_V1}/network-sim/${encodeURIComponent(env)}/partition`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ latency_ms: latencyMs ?? null }),
    });
    await throwIfNotOk(res, "Partition environment");
    return await res.json();
}

/** Heal an environment back to ALL others. */
export async function healEnv(env: string): Promise<EnvActionResponse> {
    const res = await fetch(`${API_V1}/network-sim/${encodeURIComponent(env)}/heal`, {
        method: "POST",
    });
    await throwIfNotOk(res, "Heal environment");
    return await res.json();
}

export async function partitionAll(): Promise<BulkActionResponse> {
    const res = await fetch(`${API_V1}/network-sim/partition-all`, {
        method: "POST",
    });
    await throwIfNotOk(res, "Partition all environments");
    return await res.json();
}

export async function healAll(): Promise<BulkActionResponse> {
    const res = await fetch(`${API_V1}/network-sim/heal-all`, {
        method: "POST",
    });
    await throwIfNotOk(res, "Heal all environments");
    return await res.json();
}
