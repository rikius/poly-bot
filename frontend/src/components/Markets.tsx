import type { MarketInfo } from "../types/api";

interface Props {
  markets: MarketInfo[];
}

const STATUS_META: Record<string, { label: string; cls: string }> = {
  tradeable:      { label: "TRADEABLE",   cls: "status-tradeable" },
  below_min_edge: { label: "LOW EDGE",    cls: "status-low-edge" },
  no_arb:         { label: "NO ARB",      cls: "status-no-arb" },
  thin_book:      { label: "THIN BOOK",   cls: "status-thin" },
  no_data:        { label: "NO DATA",     cls: "status-nodata" },
};

function truncId(id: string) {
  return id.length > 14 ? id.slice(0, 6) + "…" + id.slice(-6) : id;
}

function fmtPrice(v: string | null | undefined, fallback = "—") {
  return v ?? fallback;
}

export function Markets({ markets }: Props) {
  if (markets.length === 0) {
    return (
      <section className="card">
        <h2 className="card-title">Markets</h2>
        <p className="text-dim">No markets registered.</p>
      </section>
    );
  }

  return (
    <section className="card markets-card">
      <div className="markets-header">
        <h2 className="card-title" style={{ marginBottom: 0 }}>
          Markets
          <span className="markets-count">{markets.length}</span>
        </h2>
      </div>

      <div className="table-wrap">
        <table className="data-table markets-table">
          <thead>
            <tr>
              <th>Market</th>
              <th>Fee</th>
              <th className="col-price">YES bid</th>
              <th className="col-price">YES ask</th>
              <th className="col-price">NO bid</th>
              <th className="col-price">NO ask</th>
              <th className="col-price">Combined</th>
              <th className="col-price">Mid sum</th>
              <th className="col-price">Raw edge</th>
              <th>Status</th>
            </tr>
          </thead>
          <tbody>
            {markets.map((m) => {
              const meta = STATUS_META[m.status] ?? { label: m.status, cls: "status-nodata" };
              const edgeNum = m.raw_edge ? parseFloat(m.raw_edge) : null;
              const edgeCls =
                edgeNum === null ? "" :
                edgeNum > 0.03 ? "price-green" :
                edgeNum > 0 ? "price-yellow" : "price-red";

              return (
                <tr key={m.condition_id}>
                  <td>
                    <div className="market-id" title={m.condition_id}>
                      {truncId(m.condition_id)}
                    </div>
                    {m.description && (
                      <div className="market-desc">{m.description}</div>
                    )}
                  </td>
                  <td className="col-fee">{m.fee_rate_bps} bps</td>
                  <td className="col-price text-dim">{fmtPrice(m.yes_bid)}</td>
                  <td className="col-price price-green">{fmtPrice(m.yes_ask)}</td>
                  <td className="col-price text-dim">{fmtPrice(m.no_bid)}</td>
                  <td className="col-price price-red">{fmtPrice(m.no_ask)}</td>
                  <td className="col-price">
                    {m.combined_ask ?? "—"}
                  </td>
                  <td className="col-price text-secondary">
                    {m.mid_sum ?? "—"}
                  </td>
                  <td className={`col-price ${edgeCls}`}>
                    {m.raw_edge ?? "—"}
                  </td>
                  <td>
                    <span className={`status-badge ${meta.cls}`}>
                      {meta.label}
                    </span>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </section>
  );
}
