import { useWebSocket } from "./hooks/useWebSocket";
import { StatusBar } from "./components/StatusBar";
import { Portfolio } from "./components/Portfolio";
import { Positions, ActiveOrders } from "./components/ActiveTrades";
import { FillHistory } from "./components/FillHistory";
import { Controls } from "./components/Controls";
import { Markets } from "./components/Markets";

export function App() {
  const { snapshot, status, lastUpdated } = useWebSocket();

  const isConnecting = status === "connecting" && snapshot === null;

  return (
    <div className="app">
      <StatusBar
        status={status}
        botStatus={snapshot?.bot_status ?? null}
        lastUpdated={lastUpdated}
      />

      {isConnecting ? (
        <div className="loading-screen">
          <div className="spinner" />
          <p>Connecting to bot…</p>
        </div>
      ) : snapshot === null ? (
        <div className="loading-screen">
          <p>Waiting for data…</p>
        </div>
      ) : (
        <main className="main-content">
          <Controls controls={snapshot.controls} strategies={snapshot.strategies ?? []} />
          <Markets markets={snapshot.markets} />
          <Portfolio
            cash={snapshot.cash}
            pnl={snapshot.pnl}
            orderStats={snapshot.order_stats}
          />
          <Positions positions={snapshot.positions} />
          <ActiveOrders orders={snapshot.active_orders} />
          <FillHistory fills={snapshot.recent_fills} />
        </main>
      )}
    </div>
  );
}
