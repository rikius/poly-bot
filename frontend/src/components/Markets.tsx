import type { MarketInfo } from "../types/api";

interface Props {
  markets: MarketInfo[];
}

const STATUS_META: Record<string, { label: string; cls: string }> = {
  tradeable:      { label: "TRADEABLE", cls: "status-tradeable" },
  below_min_edge: { label: "LOW EDGE",  cls: "status-low-edge"  },
  no_arb:         { label: "NO ARB",    cls: "status-no-arb"    },
  thin_book:      { label: "THIN BOOK", cls: "status-thin"      },
  no_data:        { label: "NO DATA",   cls: "status-nodata"    },
};

function truncId(id: string) {
  return id.length > 14 ? `${id.slice(0, 6)}…${id.slice(-6)}` : id;
}

function fmtPrice(v: string | null | undefined, fallback = "—") {
  return v ?? fallback;
}

/** Returns YES probability 0–1 from yes_ask (the cost to buy YES) */
function yesProbability(yesAsk: string | null | undefined, noAsk: string | null | undefined): number | null {
  const y = parseFloat(yesAsk ?? "");
  const n = parseFloat(noAsk ?? "");
  if (isNaN(y) || isNaN(n) || y <= 0 || n <= 0) return null;
  // Implied probability: yes_ask / (yes_ask + no_ask)
  return y / (y + n);
}

export function Markets({ markets }: Props) {
  if (markets.length === 0) {
    return (
      <section className="card card--markets">
        <div className="card-header">
          <h2 className="card-title"><span className="card-title-icon">◫</span> Markets</h2>
        </div>
        <div className="empty-state">
          <span className="empty-state__icon">⬡</span>
          No markets registered
        </div>
      </section>
    );
  }

  return (
    <section className="card card--markets">
      <div className="markets-header">
        <h2 className="card-title" style={{ marginBottom: 0 }}>
          <span className="card-title-icon">◫</span>
          Markets
        </h2>
        <span className="markets-count">{markets.length}</span>
      </div>

      <div className="table-wrap">
        <table className="data-table markets-table">
          <thead>
            <tr>
              <th>Market / Question</th>
              <th>Fee</th>
              <th className="col-price">YES bid</th>
              <th className="col-price">YES ask</th>
              <th className="col-price">NO bid</th>
              <th className="col-price">NO ask</th>
              <th className="col-price">Combined</th>
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
                edgeNum > 0.03   ? "price-green" :
                edgeNum > 0      ? "price-yellow" : "price-red";

              const prob = yesProbability(m.yes_ask, m.no_ask);
              const yesPct = prob !== null ? Math.round(prob * 100) : null;

              return (
                <tr key={m.condition_id}>
                  <td>
                    {m.description ? (
                      <>
                        <div className="market-question" title={m.description}>
                          {m.description}
                        </div>
                        <div className="market-id-sub" title={m.condition_id}>
                          {truncId(m.condition_id)}
                        </div>
                      </>
                    ) : (
                      <div className="market-id" title={m.condition_id}>
                        {truncId(m.condition_id)}
                      </div>
                    )}
                    {yesPct !== null && (
                      <div className="prob-bar" title={`YES ~${yesPct}%`}>
                        <div
                          className="prob-bar__yes"
                          style={{ width: `${yesPct}%` }}
                        />
                      </div>
                    )}
                  </td>
                  <td className="col-fee">{m.fee_rate_bps} bps</td>
                  <td className="col-price text-secondary">{fmtPrice(m.yes_bid)}</td>
                  <td className="col-price price-green">{fmtPrice(m.yes_ask)}</td>
                  <td className="col-price text-secondary">{fmtPrice(m.no_bid)}</td>
                  <td className="col-price price-red">{fmtPrice(m.no_ask)}</td>
                  <td className="col-price">{m.combined_ask ?? "—"}</td>
                  <td className={`col-price ${edgeCls}`}>{m.raw_edge ?? "—"}</td>
                  <td>
                    <span className={`status-badge ${meta.cls}`}>{meta.label}</span>
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
