"use client";

import { useState, useEffect, useCallback } from "react";
import {
    Bug,
    RefreshCcw,
    Wifi,
    WifiOff,
    Loader2,
    Zap,
    Timer,
    ArrowLeftRight,
} from "lucide-react";
import { Input } from "@/components/ui/input";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card } from "@/components/ui/card";
import {
    fetchNetworkState,
    partitionLink,
    healLink,
    setLinkLatency,
    partitionAll,
    healAll,
    LinkState,
} from "@/lib/api";
import { toast } from "sonner";

function linkKey(link: LinkState) {
    return `${link.env_a}::${link.env_b}`;
}

export default function DebugPage() {
    const [environments, setEnvironments] = useState<string[]>([]);
    const [links, setLinks] = useState<LinkState[]>([]);
    const [loading, setLoading] = useState(true);
    const [error, setError] = useState<string | null>(null);
    const [acting, setActing] = useState<string | null>(null);
    const [latencyInputs, setLatencyInputs] = useState<Record<string, string>>({});

    const loadData = useCallback(async () => {
        try {
            setLoading(true);
            setError(null);
            const data = await fetchNetworkState();
            setEnvironments(data.environments);
            setLinks(data.links);
        } catch (e) {
            setError(e instanceof Error ? e.message : "Failed to load network state");
        } finally {
            setLoading(false);
        }
    }, []);

    useEffect(() => {
        loadData();
    }, [loadData]);

    const handlePartitionLink = async (link: LinkState) => {
        const key = linkKey(link);
        try {
            setActing(key);
            const latencyStr = latencyInputs[key];
            const latencyMs = latencyStr ? parseInt(latencyStr, 10) : undefined;
            await partitionLink(link.env_a, link.env_b, latencyMs);
            toast.success(`Partitioned link ${link.env_a} ↔ ${link.env_b}`);
            await loadData();
        } catch (e) {
            toast.error(e instanceof Error ? e.message : "Partition failed");
        } finally {
            setActing(null);
        }
    };

    const handleHealLink = async (link: LinkState) => {
        const key = linkKey(link);
        try {
            setActing(key);
            await healLink(link.env_a, link.env_b);
            toast.success(`Healed link ${link.env_a} ↔ ${link.env_b}`);
            await loadData();
        } catch (e) {
            toast.error(e instanceof Error ? e.message : "Heal failed");
        } finally {
            setActing(null);
        }
    };

    const handleSetLatency = async (link: LinkState) => {
        const key = linkKey(link);
        try {
            setActing(key);
            const latencyStr = latencyInputs[key];
            const latencyMs = latencyStr ? parseInt(latencyStr, 10) : 0;
            await setLinkLatency(link.env_a, link.env_b, latencyMs);
            toast.success(`Latency set to ${latencyMs} ms on ${link.env_a} ↔ ${link.env_b}`);
            await loadData();
        } catch (e) {
            toast.error(e instanceof Error ? e.message : "Set latency failed");
        } finally {
            setActing(null);
        }
    };

    const handleBulkPartition = async () => {
        try {
            setActing("__bulk__");
            await partitionAll();
            toast.success("All links partitioned");
            await loadData();
        } catch (e) {
            toast.error(e instanceof Error ? e.message : "Bulk partition failed");
        } finally {
            setActing(null);
        }
    };

    const handleBulkHeal = async () => {
        try {
            setActing("__bulk__");
            await healAll();
            toast.success("All links healed");
            await loadData();
        } catch (e) {
            toast.error(e instanceof Error ? e.message : "Bulk heal failed");
        } finally {
            setActing(null);
        }
    };

    const disconnectedCount = links.filter((l) => !l.connected).length;

    return (
        <div className="space-y-6">
            <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-4">
                <div>
                    <h1 className="text-3xl font-bold tracking-tight flex items-center gap-2">
                        <Bug className="h-8 w-8" /> Debug
                    </h1>
                    <p className="text-muted-foreground mt-1">
                        Network partition simulation — pairwise inter-environment links
                    </p>
                </div>
                <Button variant="outline" onClick={loadData} disabled={loading}>
                    <RefreshCcw className={`mr-2 h-4 w-4 ${loading ? "animate-spin" : ""}`} /> Refresh
                </Button>
            </div>

            {/* Bulk actions & summary */}
            <Card className="p-4">
                <div className="flex flex-col sm:flex-row items-start sm:items-center justify-between gap-4">
                    <div className="text-sm text-muted-foreground">
                        {environments.length} environment(s) &bull; {links.length} link(s) &bull;{" "}
                        <span className={disconnectedCount > 0 ? "text-destructive font-medium" : ""}>
                            {disconnectedCount} disconnected
                        </span>
                    </div>
                    <div className="flex gap-2">
                        <Button
                            variant="destructive"
                            size="sm"
                            onClick={handleBulkPartition}
                            disabled={acting !== null}
                        >
                            <WifiOff className="mr-2 h-4 w-4" /> Partition All
                        </Button>
                        <Button
                            variant="default"
                            size="sm"
                            onClick={handleBulkHeal}
                            disabled={acting !== null}
                        >
                            <Wifi className="mr-2 h-4 w-4" /> Heal All
                        </Button>
                    </div>
                </div>
            </Card>

            {/* Link cards */}
            <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-6">
                {loading ? (
                    <div className="col-span-full flex flex-col items-center justify-center py-12 text-muted-foreground space-y-4">
                        <Loader2 className="h-8 w-8 animate-spin text-primary" />
                        <p>Loading network state...</p>
                    </div>
                ) : error ? (
                    <div className="col-span-full text-center py-12 text-destructive">
                        Error: {error}
                    </div>
                ) : links.length === 0 ? (
                    <div className="col-span-full text-center py-12 text-muted-foreground">
                        No links found (need at least 2 environments)
                    </div>
                ) : (
                    links.map((link) => {
                        const key = linkKey(link);
                        return (
                            <Card key={key} className="flex flex-col">
                                {/* Header */}
                                <div className="p-6 flex items-start justify-between border-b pb-4">
                                    <div className="flex items-center gap-2">
                                        {link.connected ? (
                                            <Wifi className="h-5 w-5 text-green-600 dark:text-green-400" />
                                        ) : (
                                            <WifiOff className="h-5 w-5 text-destructive" />
                                        )}
                                        <span className="font-semibold text-lg flex items-center gap-1">
                                            {link.env_a}
                                            <ArrowLeftRight className="h-4 w-4 text-muted-foreground" />
                                            {link.env_b}
                                        </span>
                                    </div>
                                    <Badge
                                        variant={link.connected ? "success" : "destructive"}
                                        className="flex items-center gap-1"
                                    >
                                        {link.connected ? "Connected" : "Partitioned"}
                                    </Badge>
                                </div>

                                {/* Details */}
                                <div className="p-6 pt-4 flex-1 space-y-3">
                                    <div className="flex justify-between text-sm">
                                        <span className="text-muted-foreground flex items-center gap-1">
                                            <Timer className="h-3.5 w-3.5" /> Latency
                                        </span>
                                        <span className="font-mono">
                                            {link.latency_ms > 0 ? `${link.latency_ms} ms` : "None"}
                                        </span>
                                    </div>

                                    {/* Latency input + apply button */}
                                    {link.connected && (
                                        <div className="flex items-center gap-2">
                                            <Input
                                                type="number"
                                                min={0}
                                                placeholder="Latency (ms)"
                                                className="h-8 text-sm"
                                                value={latencyInputs[key] ?? ""}
                                                onChange={(e) =>
                                                    setLatencyInputs((prev) => ({
                                                        ...prev,
                                                        [key]: e.target.value,
                                                    }))
                                                }
                                            />
                                            <Button
                                                variant="outline"
                                                size="sm"
                                                className="h-8 shrink-0"
                                                onClick={() => handleSetLatency(link)}
                                                disabled={acting !== null || !latencyInputs[key]}
                                            >
                                                <Timer className="mr-1 h-3 w-3" /> Set
                                            </Button>
                                        </div>
                                    )}
                                </div>

                                {/* Actions */}
                                <div className="p-4 bg-muted/30 border-t flex justify-end gap-2">
                                    {link.connected ? (
                                        <Button
                                            variant="destructive"
                                            size="sm"
                                            className="h-8"
                                            onClick={() => handlePartitionLink(link)}
                                            disabled={acting !== null}
                                        >
                                            <Zap className="mr-2 h-3 w-3" /> Partition
                                        </Button>
                                    ) : (
                                        <Button
                                            variant="default"
                                            size="sm"
                                            className="h-8"
                                            onClick={() => handleHealLink(link)}
                                            disabled={acting !== null}
                                        >
                                            <Wifi className="mr-2 h-3 w-3" /> Heal
                                        </Button>
                                    )}
                                </div>
                            </Card>
                        );
                    })
                )}
            </div>
        </div>
    );
}
