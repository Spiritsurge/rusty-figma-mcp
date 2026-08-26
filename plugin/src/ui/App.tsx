import { useEffect, useState } from "react";

import { type Activity, formatDuration } from "./activity";
import { useLink } from "./useLink";
import { type Session, useSessions } from "./useSessions";

interface Status {
  fileName: string;
  pageName: string;
  selectionCount: number;
}

export default function App() {
  const { sessions, scanning, rescan } = useSessions();
  const [selected, setSelected] = useState<Session | null>(null);
  const { state, active, history } = useLink(selected);
  const [status, setStatus] = useState<Status | null>(null);

  // Announce on mount rather than on socket open: the panel shows which file
  // it is attached to whether or not a server was ever found.
  useEffect(() => {
    parent.postMessage({ pluginMessage: { kind: "ui-ready" } }, "*");
  }, []);

  useEffect(() => {
    const onMessage = (event: MessageEvent) => {
      const message = event.data?.pluginMessage;
      if (message?.kind === "status") {
        setStatus({
          fileName: message.fileName,
          pageName: message.pageName,
          selectionCount: message.selectionCount,
        });
      }
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  // A session that has gone away should not stay selected: the reconnect loop
  // would retry a port nothing is listening on.
  useEffect(() => {
    if (selected && !sessions.some((s) => s.port === selected.port)) {
      setSelected(null);
    }
  }, [sessions, selected]);

  return (
    <div className="app">
      <header>
        <div className="file" title={status?.fileName}>
          {status?.fileName ?? "—"}
        </div>
        <div className="sub">
          {status?.pageName ?? "—"} · {status?.selectionCount ?? 0} selected
        </div>
      </header>

      {selected ? (
        <Connected
          session={selected}
          state={state}
          active={active}
          history={history}
          onDisconnect={() => setSelected(null)}
        />
      ) : (
        <Picker
          sessions={sessions}
          scanning={scanning}
          onRescan={rescan}
          onPick={setSelected}
        />
      )}

      <footer>
        <span className="product">Rusty Figma MCP</span>
        <span className="author">made by Spiritsurge</span>
      </footer>
    </div>
  );
}

function Picker({
  sessions,
  scanning,
  onRescan,
  onPick,
}: {
  sessions: Session[];
  scanning: boolean;
  onRescan: () => void;
  onPick: (s: Session) => void;
}) {
  return (
    <>
      <div className="section-label">
        Sessions
        <button className="link-button" onClick={onRescan} disabled={scanning}>
          {scanning ? "scanning…" : "rescan"}
        </button>
      </div>

      {sessions.length === 0 ? (
        <div className="empty">
          {scanning ? (
            "Looking for servers…"
          ) : (
            <>
              <p>No server found.</p>
              <p className="hint">
                Start your MCP client with <code>figma-mcp</code> configured, then rescan.
              </p>
            </>
          )}
        </div>
      ) : (
        <ul className="sessions">
          {sessions.map((session) => (
            <li key={session.port}>
              <button className="session" onClick={() => onPick(session)}>
                <span className="label">{session.label}</span>
                {session.connected ? (
                  <span className="in-use" title="Another Figma window is using this session">
                    in use
                  </span>
                ) : (
                  <span className="port">:{session.port}</span>
                )}
              </button>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

function Connected({
  session,
  state,
  active,
  history,
  onDisconnect,
}: {
  session: Session;
  state: string;
  active: Activity[];
  history: Activity[];
  onDisconnect: () => void;
}) {
  const idle = state === "connected" && active.length === 0;

  return (
    <div className="connected">
      <div className={`badge ${state}`}>
        <span className="dot" />
        {state === "connected"
          ? active.length > 0
            ? `working — ${active.length} request${active.length > 1 ? "s" : ""}`
            : "connected"
          : state === "connecting"
            ? "connecting…"
            : state === "ended"
              ? "session ended"
              : "connection lost — retrying"}
      </div>

      <div className="target">
        <div className="label">{session.label}</div>
        <div className="port">localhost:{session.port}</div>
      </div>

      <div className="activity">
        {active.map((item) => (
          <ActivityRow key={item.id} item={item} running />
        ))}
        {history.map((item) => (
          <ActivityRow key={item.id} item={item} />
        ))}
        {idle && history.length === 0 && (
          <p className="hint idle">Waiting for the assistant…</p>
        )}
      </div>

      <button className="disconnect" onClick={onDisconnect}>
        {state === "ended" ? "Back to sessions" : "Disconnect"}
      </button>
    </div>
  );
}

function ActivityRow({ item, running = false }: { item: Activity; running?: boolean }) {
  return (
    <div className={`activity-row ${running ? "running" : item.outcome}`}>
      <span className="marker" />
      <span className="what" title={item.note ?? item.method}>
        {item.label}
      </span>
      <span className="when">
        {running
          ? item.pct !== undefined
            ? `${item.pct}%`
            : "…"
          : item.ms !== undefined
            ? formatDuration(item.ms)
            : ""}
      </span>
    </div>
  );
}
