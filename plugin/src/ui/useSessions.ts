import { useCallback, useEffect, useState } from "react";

// PROTOCOL.md §4: the iframe has no filesystem access, so servers are found by
// probing the port range over the network the iframe does have.
const PORT_RANGE = Array.from({ length: 20 }, (_, i) => 51820 + i);
const PROBE_TIMEOUT_MS = 400;

export interface Session {
  port: number;
  v: number;
  host: string;
  pid: number;
  label: string;
  started_at_ms: number;
}

async function probe(port: number): Promise<Session | null> {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), PROBE_TIMEOUT_MS);
  try {
    const response = await fetch(`http://127.0.0.1:${port}/hello`, { signal: abort.signal });
    if (!response.ok) return null;
    const identity = (await response.json()) as Omit<Session, "port">;
    return { ...identity, port };
  } catch {
    // A closed port is the overwhelmingly common case, not an error worth
    // surfacing — twenty of these fire on every scan.
    return null;
  } finally {
    clearTimeout(timer);
  }
}

export function useSessions() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [scanning, setScanning] = useState(false);

  const scan = useCallback(async () => {
    setScanning(true);
    const found = await Promise.all(PORT_RANGE.map(probe));
    setSessions(
      found
        .filter((s): s is Session => s !== null)
        // Newest first: the server just started is the one being connected.
        .sort((a, b) => b.started_at_ms - a.started_at_ms),
    );
    setScanning(false);
  }, []);

  useEffect(() => {
    void scan();
  }, [scan]);

  return { sessions, scanning, rescan: scan };
}
