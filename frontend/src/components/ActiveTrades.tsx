import type { PositionInfo, OrderInfo } from "../types/api";

interface PositionsProps {
  positions: PositionInfo[];
}

interface OrdersProps {
  orders: OrderInfo[];
}

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

export function Positions({ positions }: PositionsProps) {
  return (
    <section className="card">
      <h2 className="card-title">
        Open Positions
        <span className="badge">{positions.length}</span>
      </h2>
      {positions.length === 0 ? (
        <p className="empty-state">No open positions</p>
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
                <th>Realized P&L</th>
                <th>Unrealized P&L</th>
                <th>Total P&L</th>
              </tr>
            </thead>
            <tbody>
              {positions.map((p) => (
                <tr key={p.token_id}>
                  <td className="mono token-cell" title={p.token_id}>
                    {shortToken(p.token_id)}
                  </td>
                  <td>
                    <span className={`dir-badge dir-badge--${p.direction}`}>
                      {p.direction.toUpperCase()}
                    </span>
                  </td>
                  <td className="mono">{parseFloat(p.shares).toFixed(4)}</td>
                  <td className="mono">${parseFloat(p.avg_cost).toFixed(4)}</td>
                  <td className="mono">${parseFloat(p.notional).toFixed(4)}</td>
                  <td className={`mono ${pnlColor(p.realized_pnl)}`}>
                    {fmt4(p.realized_pnl)}
                  </td>
                  <td className={`mono ${pnlColor(p.unrealized_pnl)}`}>
                    {fmt4(p.unrealized_pnl)}
                  </td>
                  <td className={`mono ${pnlColor(p.total_pnl)}`}>
                    {fmt4(p.total_pnl)}
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

const STATE_COLORS: Record<string, string> = {
  acked: "#3b82f6",
  partial: "#f59e0b",
  cancel_pending: "#ef4444",
};

export function ActiveOrders({ orders }: OrdersProps) {
  return (
    <section className="card">
      <h2 className="card-title">
        Active Orders
        <span className="badge">{orders.length}</span>
      </h2>
      {orders.length === 0 ? (
        <p className="empty-state">No active orders</p>
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
                  <td className="mono token-cell" title={o.token_id}>
                    {shortToken(o.token_id)}
                  </td>
                  <td>
                    <span className={`side-badge side-badge--${o.side}`}>
                      {o.side.toUpperCase()}
                    </span>
                  </td>
                  <td className="mono">${parseFloat(o.price).toFixed(4)}</td>
                  <td className="mono">{parseFloat(o.original_size).toFixed(2)}</td>
                  <td className="mono">{parseFloat(o.filled_size).toFixed(2)}</td>
                  <td className="mono">{parseFloat(o.remaining_size).toFixed(2)}</td>
                  <td>
                    <span
                      className="state-badge"
                      style={{ color: STATE_COLORS[o.state] ?? "#9ca3af" }}
                    >
                      {o.state}
                    </span>
                  </td>
                  <td className="mono dim">{o.strategy_id ?? "—"}</td>
                  <td className="mono dim">
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
