import type { BotStatus } from "../types/api";

type ConnectionStatus = "connecting" | "connected" | "disconnected" | "error";

interface Props {
  status: ConnectionStatus;
  botStatus: BotStatus | null;
  lastUpdated: Date | null;
}

function formatUptime(secs: number): string {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

const STATUS_COLORS: Record<ConnectionStatus, string> = {
  connecting:   "#f5c542",
  connected:    "#00e59b",
  disconnected: "#3a4560",
  error:        "#ff3b5c",
};

export function StatusBar({ status, botStatus, lastUpdated }: Props) {
  const isLive = botStatus?.mode === "live";

  return (
    <header className={`status-bar${isLive ? " status-bar--live" : ""}`}>
      <div className="status-bar-left">
        <span className="logo">PolyBot</span>
        {botStatus && (
          <span className={`mode-badge mode-badge--${botStatus.mode}`}>
            {botStatus.mode.toUpperCase()}
          </span>
        )}
        {botStatus && (
          <div className="status-bar-stat">
            <span className="status-bar-stat__label">Uptime</span>
            <span className="status-bar-stat__value" style={{ color: "var(--text-secondary)" }}>
              {formatUptime(botStatus.uptime_secs)}
            </span>
          </div>
        )}
      </div>

      <div className="status-bar-right">
        {lastUpdated && (
          <span className="last-updated">
            {lastUpdated.toLocaleTimeString()}
          </span>
        )}
        <span
          className="connection-indicator"
          style={{ color: STATUS_COLORS[status] }}
        >
          <span className={`dot${status === "connected" ? " dot--connected" : ""}`} />
          {status.charAt(0).toUpperCase() + status.slice(1)}
        </span>
      </div>
    </header>
  );
}
