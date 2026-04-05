import type { CashInfo, PnlInfo, OrderStatsInfo } from "../types/api";

interface Props {
  cash: CashInfo;
  pnl: PnlInfo;
  orderStats: OrderStatsInfo;
}

function Metric({ label, value, positive }: { label: string; value: string; positive?: boolean | null }) {
  const colorClass = positive === true ? "value--positive" : positive === false ? "value--negative" : "";
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

function heroSign(val: string): string {
  const n = parseFloat(val);
  if (isNaN(n)) return "";
  if (n > 0) return "value--positive";
  if (n < 0) return "value--negative";
  return "";
}

export function Portfolio({ cash, pnl, orderStats }: Props) {
  const netFormatted = fmt(pnl.net);
  const netClass = heroSign(pnl.net);
  const totalCash = fmtUsd(cash.total);

  return (
    <section className="card card--portfolio">
      <div className="card-header">
        <h2 className="card-title">
          <span className="card-title-icon">◈</span>
          Portfolio
        </h2>
        <span style={{ fontSize: 10, color: "var(--text-dim)", fontFamily: "var(--font-mono)", letterSpacing: "0.06em" }}>
          {orderStats.active_count} ACTIVE ORDER{orderStats.active_count !== 1 ? "S" : ""}
        </span>
      </div>

      {/* Hero stats */}
      <div className="portfolio-hero">
        <div className="hero-stat">
          <span className="hero-stat__label">Net P&amp;L</span>
          <span className={`hero-stat__value ${netClass}`}>{netFormatted}</span>
          <span className="hero-stat__sub">
            Realized {fmt(pnl.realized)} · Unrealized {fmt(pnl.unrealized)}
          </span>
        </div>
        <div className="hero-stat">
          <span className="hero-stat__label">Total Cash</span>
          <span className="hero-stat__value">{totalCash}</span>
          <span className="hero-stat__sub">
            Available {fmtUsd(cash.available)} · Reserved {fmtUsd(cash.reserved)}
          </span>
        </div>
      </div>

      {/* Detailed metrics */}
      <div className="portfolio-metrics">
        <div className="metrics-group">
          <h3 className="group-title">Cash</h3>
          <Metric label="Available" value={fmtUsd(cash.available)} />
          <Metric label="Reserved"  value={fmtUsd(cash.reserved)} />
          <Metric label="Total"     value={fmtUsd(cash.total)} />
        </div>

        <div className="metrics-group">
          <h3 className="group-title">P&amp;L</h3>
          <Metric label="Realized"   value={fmt(pnl.realized)}   positive={pnlSign(pnl.realized)} />
          <Metric label="Unrealized" value={fmt(pnl.unrealized)} positive={pnlSign(pnl.unrealized)} />
          <Metric label="Net"        value={fmt(pnl.net)}        positive={pnlSign(pnl.net)} />
          <Metric label="Fees Paid"  value={`-$${parseFloat(pnl.total_fees).toFixed(4)}`} positive={null} />
        </div>

        <div className="metrics-group">
          <h3 className="group-title">Orders</h3>
          <Metric label="Active"    value={String(orderStats.active_count)} />
          <Metric label="Filled"    value={String(orderStats.total_filled)} />
          <Metric label="Cancelled" value={String(orderStats.total_cancelled)} />
          <Metric label="Rejected"  value={String(orderStats.total_rejected)} />
        </div>
      </div>
    </section>
  );
}
