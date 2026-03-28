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
  if (h > 0) return `${h}h ${m}m ${s}s`;
  if (m > 0) return `${m}m ${s}s`;
  return `${s}s`;
}

const STATUS_COLORS: Record<ConnectionStatus, string> = {
  connecting: "#f59e0b",
  connected: "#10b981",
  disconnected: "#6b7280",
  error: "#ef4444",
};

export function StatusBar({ status, botStatus, lastUpdated }: Props) {
  return (
    <header className="status-bar">
      <div className="status-bar-left">
        <span className="logo">PolyBot</span>
        {botStatus && (
          <span className={`mode-badge mode-badge--${botStatus.mode}`}>
            {botStatus.mode.toUpperCase()}
          </span>
        )}
      </div>

      <div className="status-bar-right">
        {botStatus && (
          <span className="uptime">
            Uptime: {formatUptime(botStatus.uptime_secs)}
          </span>
        )}
        {lastUpdated && (
          <span className="last-updated">
            {lastUpdated.toLocaleTimeString()}
          </span>
        )}
        <span
          className="connection-indicator"
          style={{ color: STATUS_COLORS[status] }}
        >
          <span className="dot" />
          {status.charAt(0).toUpperCase() + status.slice(1)}
        </span>
      </div>
    </header>
  );
}
