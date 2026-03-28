import type { CashInfo, PnlInfo, OrderStatsInfo } from "../types/api";

interface Props {
  cash: CashInfo;
  pnl: PnlInfo;
  orderStats: OrderStatsInfo;
}

function Metric({
  label,
  value,
  positive,
}: {
  label: string;
  value: string;
  positive?: boolean | null;
}) {
  const colorClass =
    positive === true
      ? "value--positive"
      : positive === false
        ? "value--negative"
        : "";
  return (
    <div className="metric">
      <span className="metric-label">{label}</span>
      <span className={`metric-value ${colorClass}`}>{value}</span>
    </div>
  );
}

function pnlSign(val: string): boolean | null {
  const n = parseFloat(val);
  if (n > 0) return true;
  if (n < 0) return false;
  return null;
}

function fmt(val: string): string {
  const n = parseFloat(val);
  if (isNaN(n)) return val;
  const prefix = n >= 0 ? "+" : "";
  return `${prefix}$${Math.abs(n).toFixed(4)}`;
}

function fmtUsd(val: string): string {
  const n = parseFloat(val);
  if (isNaN(n)) return val;
  return `$${n.toFixed(2)}`;
}

export function Portfolio({ cash, pnl, orderStats }: Props) {
  return (
    <section className="card">
      <h2 className="card-title">Portfolio</h2>
      <div className="metrics-grid">
        <div className="metrics-group">
          <h3 className="group-title">Cash</h3>
          <Metric label="Available" value={fmtUsd(cash.available)} />
          <Metric label="Reserved" value={fmtUsd(cash.reserved)} />
          <Metric label="Total" value={fmtUsd(cash.total)} />
        </div>

        <div className="metrics-group">
          <h3 className="group-title">P&amp;L</h3>
          <Metric
            label="Realized"
            value={fmt(pnl.realized)}
            positive={pnlSign(pnl.realized)}
          />
          <Metric
            label="Unrealized"
            value={fmt(pnl.unrealized)}
            positive={pnlSign(pnl.unrealized)}
          />
          <Metric
            label="Net P&amp;L"
            value={fmt(pnl.net)}
            positive={pnlSign(pnl.net)}
          />
          <Metric label="Fees Paid" value={`-$${parseFloat(pnl.total_fees).toFixed(4)}`} positive={null} />
        </div>

        <div className="metrics-group">
          <h3 className="group-title">Orders</h3>
          <Metric label="Active" value={String(orderStats.active_count)} />
          <Metric label="Filled" value={String(orderStats.total_filled)} />
          <Metric label="Cancelled" value={String(orderStats.total_cancelled)} />
          <Metric label="Rejected" value={String(orderStats.total_rejected)} />
        </div>
      </div>
    </section>
  );
}
