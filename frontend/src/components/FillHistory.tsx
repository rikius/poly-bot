import type { FillInfo } from "../types/api";

interface Props {
  fills: FillInfo[];
}

function shortToken(id: string): string {
  if (id.length <= 12) return id;
  return `${id.slice(0, 6)}…${id.slice(-6)}`;
}

export function FillHistory({ fills }: Props) {
  return (
    <section className="card">
      <h2 className="card-title">
        Recent Fills
        <span className="badge">{fills.length}</span>
      </h2>
      {fills.length === 0 ? (
        <p className="empty-state">No fills yet</p>
      ) : (
        <div className="table-wrapper">
          <table className="data-table">
            <thead>
              <tr>
                <th>Time</th>
                <th>Token</th>
                <th>Side</th>
                <th>Price</th>
                <th>Size</th>
                <th>Notional</th>
                <th>Fee</th>
                <th>Order ID</th>
              </tr>
            </thead>
            <tbody>
              {fills.map((f) => (
                <tr key={f.fill_id}>
                  <td className="mono dim">
                    {new Date(f.timestamp).toLocaleTimeString()}
                  </td>
                  <td className="mono token-cell" title={f.token_id}>
                    {shortToken(f.token_id)}
                  </td>
                  <td>
                    <span className={`side-badge side-badge--${f.side}`}>
                      {f.side.toUpperCase()}
                    </span>
                  </td>
                  <td className="mono">${parseFloat(f.price).toFixed(4)}</td>
                  <td className="mono">{parseFloat(f.size).toFixed(4)}</td>
                  <td className="mono">${parseFloat(f.notional).toFixed(4)}</td>
                  <td className="mono dim">${parseFloat(f.fee).toFixed(6)}</td>
                  <td className="mono dim" title={f.order_id}>
                    {f.order_id.slice(0, 12)}…
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
