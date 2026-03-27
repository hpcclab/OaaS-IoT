/**
 * Tests for env-aware gateway API functions in src/lib/api.ts.
 *
 * Covers:
 * - fetchObjects: without env (default gateway) and with env (per-env gateway)
 * - fetchObject: default and per-env routing
 * - createOrUpdateObject: body forwarding + env routing
 * - deleteObject: default and per-env routing
 * - invokeFunction: default and per-env routing + body shape
 */
import { describe, it, expect, vi, afterEach } from "vitest";
import {
    fetchObjects,
    fetchObject,
    createOrUpdateObject,
    deleteObject,
    invokeFunction,
    encodeEntries,
} from "@/lib/api";

// ---------------------------------------------------------------------------
// Fetch mock helpers
// ---------------------------------------------------------------------------

const originalFetch = globalThis.fetch;

function mockFetch(handler: (url: string, init?: RequestInit) => Promise<Response>) {
    globalThis.fetch = vi.fn(handler) as unknown as typeof fetch;
}

afterEach(() => {
    globalThis.fetch = originalFetch;
});

// ---------------------------------------------------------------------------
// fetchObjects
// ---------------------------------------------------------------------------

describe("fetchObjects", () => {
    it("uses default gateway path when no env is provided", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({ objects: [{ object_id: "obj-1", version: 1, entry_count: 0 }] }), {
                status: 200,
                headers: { "Content-Type": "application/json" },
            });
        });

        const result = await fetchObjects("my.Cls", 0);
        expect(capturedUrl).toContain("/api/gateway/api/class/my.Cls/0/objects");
        expect(capturedUrl).not.toContain("/env/");
        expect(result).toHaveLength(1);
        expect(result[0].id).toBe("obj-1");
        expect(result[0].version).toBe(1);
    });

    it("uses per-env gateway path when env is provided", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({ objects: [{ object_id: "obj-2", version: 3, entry_count: 2 }] }), {
                status: 200,
                headers: { "Content-Type": "application/json" },
            });
        });

        const result = await fetchObjects("my.Cls", 0, "edge");
        expect(capturedUrl).toContain("/api/gateway/env/edge/api/class/my.Cls/0/objects");
        expect(result).toHaveLength(1);
        expect(result[0].id).toBe("obj-2");
        expect(result[0].entry_count).toBe(2);
    });

    it("returns empty array on failure", async () => {
        mockFetch(async () => new Response("", { status: 500 }));

        const result = await fetchObjects("my.Cls", 0, "cloud");
        expect(result).toEqual([]);
    });
});

// ---------------------------------------------------------------------------
// fetchObject
// ---------------------------------------------------------------------------

describe("fetchObject", () => {
    it("uses default path without env", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({ id: "obj-1", entries: {} }), {
                status: 200,
                headers: { "Content-Type": "application/json" },
            });
        });

        const result = await fetchObject("my.Cls", 0, "obj-1");
        expect(capturedUrl).toContain("/api/gateway/api/class/my.Cls/0/objects/obj-1");
        expect(capturedUrl).not.toContain("/env/");
        expect(result).toEqual({ id: "obj-1", entries: {} });
    });

    it("uses per-env path with env", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({ id: "obj-1" }), {
                status: 200,
                headers: { "Content-Type": "application/json" },
            });
        });

        await fetchObject("my.Cls", 0, "obj-1", "cloud");
        expect(capturedUrl).toContain("/api/gateway/env/cloud/api/class/my.Cls/0/objects/obj-1");
    });

    it("throws on non-ok response", async () => {
        mockFetch(async () =>
            new Response(JSON.stringify({ error: "not found" }), { status: 404 }),
        );

        await expect(fetchObject("my.Cls", 0, "missing")).rejects.toThrow();
    });
});

// ---------------------------------------------------------------------------
// createOrUpdateObject
// ---------------------------------------------------------------------------

describe("createOrUpdateObject", () => {
    it("uses default path without env", async () => {
        let capturedUrl = "";
        let capturedMethod = "";
        let capturedBody: unknown;
        mockFetch(async (url, init) => {
            capturedUrl = url;
            capturedMethod = init?.method ?? "";
            capturedBody = JSON.parse(init?.body as string);
            return new Response("", { status: 200 });
        });

        const entries = encodeEntries({ count: 5 });
        await createOrUpdateObject("my.Cls", 0, "obj-1", { entries });
        expect(capturedUrl).toContain("/api/gateway/api/class/my.Cls/0/objects/obj-1");
        expect(capturedMethod).toBe("PUT");
        expect(capturedBody).toEqual({ entries: { count: { data: btoa(JSON.stringify(5)), type: 0 } } });
    });

    it("uses per-env path with env", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response("", { status: 200 });
        });

        await createOrUpdateObject("my.Cls", 0, "obj-1", { entries: {} }, "edge");
        expect(capturedUrl).toContain("/api/gateway/env/edge/api/class/my.Cls/0/objects/obj-1");
    });

    it("throws on failure", async () => {
        mockFetch(async () => new Response("", { status: 500 }));
        await expect(
            createOrUpdateObject("my.Cls", 0, "obj-1", {}),
        ).rejects.toThrow();
    });
});

// ---------------------------------------------------------------------------
// deleteObject
// ---------------------------------------------------------------------------

describe("deleteObject", () => {
    it("uses default path without env", async () => {
        let capturedUrl = "";
        let capturedMethod = "";
        mockFetch(async (url, init) => {
            capturedUrl = url;
            capturedMethod = init?.method ?? "";
            return new Response("", { status: 200 });
        });

        await deleteObject("my.Cls", 0, "obj-1");
        expect(capturedUrl).toContain("/api/gateway/api/class/my.Cls/0/objects/obj-1");
        expect(capturedMethod).toBe("DELETE");
    });

    it("uses per-env path with env", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response("", { status: 200 });
        });

        await deleteObject("my.Cls", 0, "obj-1", "cloud");
        expect(capturedUrl).toContain("/api/gateway/env/cloud/api/class/my.Cls/0/objects/obj-1");
    });
});

// ---------------------------------------------------------------------------
// invokeFunction
// ---------------------------------------------------------------------------

describe("invokeFunction", () => {
    it("uses default path without env", async () => {
        let capturedUrl = "";
        let capturedBody: unknown;
        mockFetch(async (url, init) => {
            capturedUrl = url;
            capturedBody = JSON.parse(init?.body as string);
            return new Response(JSON.stringify({ result: "ok" }), {
                status: 200,
                headers: { "Content-Type": "application/json" },
            });
        });

        const res = await invokeFunction("my.Cls", 0, "obj-1", "echo", { msg: "hi" });
        expect(capturedUrl).toContain("/api/gateway/api/class/my.Cls/0/objects/obj-1/invokes/echo");
        expect(capturedUrl).not.toContain("/env/");
        expect(capturedBody).toEqual({ msg: "hi" });
        expect(res).toEqual({ result: "ok" });
    });

    it("uses per-env path with env", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({ result: "ok" }), {
                status: 200,
                headers: { "Content-Type": "application/json" },
            });
        });

        await invokeFunction("my.Cls", 0, "obj-1", "echo", {}, "edge");
        expect(capturedUrl).toContain("/api/gateway/env/edge/api/class/my.Cls/0/objects/obj-1/invokes/echo");
    });

    it("throws on non-ok response", async () => {
        mockFetch(async () => new Response("bad request", { status: 400 }));
        await expect(
            invokeFunction("my.Cls", 0, "obj-1", "echo", {}),
        ).rejects.toThrow("Invocation failed");
    });

    it("encodes env name in URL", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({}), { status: 200 });
        });

        await invokeFunction("my.Cls", 0, "obj-1", "fn", {}, "us-west-2");
        expect(capturedUrl).toContain("/api/gateway/env/us-west-2/api/class/");
    });
});

// ---------------------------------------------------------------------------
// encodeEntries
// ---------------------------------------------------------------------------

describe("encodeEntries", () => {
    it("encodes plain JSON values to ValData format", () => {
        const result = encodeEntries({ name: "hello", count: 42 });
        expect(result.name).toEqual({ data: btoa(JSON.stringify("hello")), type: 0 });
        expect(result.count).toEqual({ data: btoa(JSON.stringify(42)), type: 0 });
    });

    it("returns empty object for empty input", () => {
        expect(encodeEntries({})).toEqual({});
    });

    it("encodes nested objects", () => {
        const result = encodeEntries({ nested: { a: 1, b: [2, 3] } });
        expect(result.nested).toEqual({ data: btoa(JSON.stringify({ a: 1, b: [2, 3] })), type: 0 });
    });
});

// ---------------------------------------------------------------------------
// gatewayUrl (per-port) override
// ---------------------------------------------------------------------------

describe("gatewayUrl override", () => {
    it("fetchObjects uses gatewayUrl when provided", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({ objects: [] }), { status: 200 });
        });

        await fetchObjects("my.Cls", 0, "edge", "http://localhost:8082");
        expect(capturedUrl).toBe("http://localhost:8082/api/class/my.Cls/0/objects");
    });

    it("invokeFunction uses gatewayUrl when provided", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response(JSON.stringify({}), { status: 200 });
        });

        await invokeFunction("my.Cls", 0, "obj-1", "echo", {}, "cloud", "http://localhost:8081");
        expect(capturedUrl).toBe("http://localhost:8081/api/class/my.Cls/0/objects/obj-1/invokes/echo");
    });

    it("deleteObject uses gatewayUrl when provided", async () => {
        let capturedUrl = "";
        mockFetch(async (url) => {
            capturedUrl = url;
            return new Response("", { status: 200 });
        });

        await deleteObject("my.Cls", 0, "obj-1", "cloud", "http://localhost:8081");
        expect(capturedUrl).toBe("http://localhost:8081/api/class/my.Cls/0/objects/obj-1");
    });
});
