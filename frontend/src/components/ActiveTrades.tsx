import type { PositionInfo, OrderInfo } from "../types/api";

interface PositionsProps { positions: PositionInfo[]; }
interface OrdersProps    { orders: OrderInfo[]; }

function shortToken(id: string): string {
  if (id.length <= 12) return id;
  return `${id.slice(0, 6)}…${id.slice(-6)}`;
}

function pnlColor(val: string): string {
  const n = parseFloat(val);
  if (n > 0) return "value--positive";
  if (n < 0) return "value--negative";
  return "";
}

function fmt4(val: string): string {
  const n = parseFloat(val);
  if (isNaN(n)) return val;
  const prefix = n >= 0 ? "+" : "";
  return `${prefix}${n.toFixed(4)}`;
}

/** Returns 0–100 clamped fill percentage */
function pnlBarPct(val: string, notional: string): number {
  const pnl = parseFloat(val);
  const cost = parseFloat(notional);
  if (isNaN(pnl) || isNaN(cost) || cost <= 0) return 0;
  return Math.min(Math.abs(pnl / cost) * 100, 100);
}

export function Positions({ positions }: PositionsProps) {
  return (
    <section className="card card--positions">
      <div className="card-header">
        <h2 className="card-title">
          <span className="card-title-icon">◈</span>
          Open Positions
        </h2>
        <span className="badge">{positions.length}</span>
      </div>

      {positions.length === 0 ? (
        <div className="empty-state">
          <span className="empty-state__icon">◌</span>
          No open positions
        </div>
      ) : (
        <div className="table-wrapper">
          <table className="data-table">
            <thead>
              <tr>
                <th>Token</th>
                <th>Dir</th>
                <th>Shares</th>
                <th>Avg Cost</th>
                <th>Notional</th>
                <th>Realized P&amp;L</th>
                <th>Unrealized P&amp;L</th>
                <th>Total P&amp;L</th>
              </tr>
            </thead>
            <tbody>
              {positions.map((p) => {
                const totalPnl = parseFloat(p.total_pnl);
                const isPos = totalPnl > 0;
                const pct = pnlBarPct(p.total_pnl, p.notional);

                return (
                  <tr key={p.token_id}>
                    <td className="token-cell" title={p.token_id}>
                      {shortToken(p.token_id)}
                    </td>
                    <td>
                      <span className={`dir-badge dir-badge--${p.direction}`}>
                        {p.direction.toUpperCase()}
                      </span>
                    </td>
                    <td>{parseFloat(p.shares).toFixed(4)}</td>
                    <td>${parseFloat(p.avg_cost).toFixed(4)}</td>
                    <td>${parseFloat(p.notional).toFixed(4)}</td>
                    <td className={pnlColor(p.realized_pnl)}>
                      {fmt4(p.realized_pnl)}
                    </td>
                    <td className={pnlColor(p.unrealized_pnl)}>
                      {fmt4(p.unrealized_pnl)}
                    </td>
                    <td>
                      <div className={pnlColor(p.total_pnl)} style={{ fontWeight: 600 }}>
                        {fmt4(p.total_pnl)}
                      </div>
                      <div className="pnl-bar">
                        <div
                          className={`pnl-bar__fill ${isPos ? "pnl-bar__fill--pos" : "pnl-bar__fill--neg"}`}
                          style={{ width: `${pct}%` }}
                        />
                      </div>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

const STATE_COLORS: Record<string, string> = {
  acked:          "var(--cyan)",
  partial:        "var(--yellow)",
  cancel_pending: "var(--red)",
};

export function ActiveOrders({ orders }: OrdersProps) {
  return (
    <section className="card card--orders">
      <div className="card-header">
        <h2 className="card-title">
          <span className="card-title-icon">⟡</span>
          Active Orders
        </h2>
        <span className="badge">{orders.length}</span>
      </div>

      {orders.length === 0 ? (
        <div className="empty-state">
          <span className="empty-state__icon">◌</span>
          No active orders
        </div>
      ) : (
        <div className="table-wrapper">
          <table className="data-table">
            <thead>
              <tr>
                <th>Token</th>
                <th>Side</th>
                <th>Price</th>
                <th>Size</th>
                <th>Filled</th>
                <th>Remaining</th>
                <th>State</th>
                <th>Strategy</th>
                <th>Created</th>
              </tr>
            </thead>
            <tbody>
              {orders.map((o) => (
                <tr key={o.local_id}>
                  <td className="token-cell" title={o.token_id}>
                    {shortToken(o.token_id)}
                  </td>
                  <td>
                    <span className={`side-badge side-badge--${o.side.toLowerCase()}`}>
                      {o.side.toUpperCase()}
                    </span>
                  </td>
                  <td>${parseFloat(o.price).toFixed(4)}</td>
                  <td>{parseFloat(o.original_size).toFixed(2)}</td>
                  <td>{parseFloat(o.filled_size).toFixed(2)}</td>
                  <td>{parseFloat(o.remaining_size).toFixed(2)}</td>
                  <td>
                    <span
                      className="state-badge"
                      style={{ color: STATE_COLORS[o.state] ?? "var(--text-secondary)" }}
                    >
                      {o.state}
                    </span>
                  </td>
                  <td className="dim">{o.strategy_id ?? "—"}</td>
                  <td className="dim">
                    {new Date(o.created_at).toLocaleTimeString()}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
