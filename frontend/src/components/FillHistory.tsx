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
    <section className="card card--fills">
      <div className="card-header">
        <h2 className="card-title">
          <span className="card-title-icon">⟳</span>
          Recent Fills
        </h2>
        <span className="badge">{fills.length}</span>
      </div>

      {fills.length === 0 ? (
        <div className="empty-state">
          <span className="empty-state__icon">◌</span>
          No fills yet
        </div>
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
                <th>Total</th>
                <th>Fee</th>
                <th>Order ID</th>
              </tr>
            </thead>
            <tbody>
              {fills.map((f) => (
                <tr key={f.fill_id} className={`fill-row--${f.side.toLowerCase()}`}>
                  <td className="dim">
                    {new Date(f.timestamp).toLocaleTimeString()}
                  </td>
                  <td className="token-cell" title={f.token_id}>
                    {shortToken(f.token_id)}
                  </td>
                  <td>
                    <span className={`side-badge side-badge--${f.side.toLowerCase()}`}>
                      {f.side.toUpperCase()}
                    </span>
                  </td>
                  <td>${parseFloat(f.price).toFixed(4)}</td>
                  <td>{parseFloat(f.size).toFixed(4)}</td>
                  <td style={{ fontWeight: 600 }}>
                    ${parseFloat(f.notional).toFixed(4)}
                  </td>
                  <td className="dim">${parseFloat(f.fee).toFixed(6)}</td>
                  <td className="dim" title={f.order_id}>
                    {f.order_id.slice(0, 10)}…
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
