import { useState } from "react";
import { api } from "../api";
import type { ControlsInfo, StrategyInfo } from "../types/api";

interface Props {
  controls: ControlsInfo;
  strategies: StrategyInfo[];
}

export function Controls({ controls, strategies }: Props) {
  const [busy, setBusy] = useState<string | null>(null);
  const [strategyBusy, setStrategyBusy] = useState<string | null>(null);
  const [localCfg, setLocalCfg] = useState<ControlsInfo>(controls);
  const [saved, setSaved] = useState(false);
  const [cancelResult, setCancelResult] = useState<string | null>(null);

  // Keep local form in sync when snapshot updates (unless user is editing)
  // We deliberately don't sync while user is mid-edit — form is the source of
  // truth until they hit Save.

  async function handleToggle() {
    setBusy("toggle");
    try {
      if (controls.trading_paused) {
        await api.resumeBot();
      } else {
        await api.pauseBot();
      }
    } finally {
      setBusy(null);
    }
  }

  async function handleCancelAll() {
    if (!confirm("Cancel ALL open orders on the exchange?")) return;
    setBusy("cancel");
    setCancelResult(null);
    try {
      const res = await api.cancelAllOrders();
      if (res.ok) {
        setCancelResult(`Cancelled ${res.cancelled ?? 0} order(s)`);
      } else {
        setCancelResult(`Error: ${res.error ?? "unknown"}`);
      }
    } finally {
      setBusy(null);
    }
  }

  async function handleSaveConfig() {
    setBusy("save");
    setSaved(false);
    try {
      await api.patchConfig({
        max_bet_usd: parseFloat(localCfg.max_bet_usd),
        max_position_per_market_usd: parseFloat(localCfg.max_position_per_market_usd),
        max_total_exposure_usd: parseFloat(localCfg.max_total_exposure_usd),
        max_daily_loss_usd: parseFloat(localCfg.max_daily_loss_usd),
        max_open_orders: localCfg.max_open_orders,
        use_maker_mode: localCfg.use_maker_mode,
        temporal_arb_enabled: localCfg.temporal_arb_enabled,
        temporal_arb_threshold_bps: localCfg.temporal_arb_threshold_bps,
        temporal_arb_sensitivity_bps: localCfg.temporal_arb_sensitivity_bps,
      });
      setSaved(true);
      setTimeout(() => setSaved(false), 2500);
    } finally {
      setBusy(null);
    }
  }

  async function handleStrategyToggle(name: string, currentlyEnabled: boolean) {
    setStrategyBusy(name);
    try {
      if (currentlyEnabled) {
        await api.disableStrategy(name);
      } else {
        await api.enableStrategy(name);
      }
    } finally {
      setStrategyBusy(null);
    }
  }

  function num(field: keyof ControlsInfo) {
    return (e: React.ChangeEvent<HTMLInputElement>) =>
      setLocalCfg((c) => ({ ...c, [field]: e.target.value }));
  }

  function numInt(field: keyof ControlsInfo) {
    return (e: React.ChangeEvent<HTMLInputElement>) =>
      setLocalCfg((c) => ({ ...c, [field]: parseInt(e.target.value, 10) || 0 }));
  }

  function bool(field: keyof ControlsInfo) {
    return (e: React.ChangeEvent<HTMLInputElement>) =>
      setLocalCfg((c) => ({ ...c, [field]: e.target.checked }));
  }

  const paused = controls.trading_paused;

  return (
    <section className="card card--controls">
      {/* ── Header ── */}
      <div className="card-header">
        <h2 className="card-title">
          <span className="card-title-icon">⌬</span>
          Bot Controls
        </h2>
        <span className={`bot-status-badge ${paused ? "paused" : "active"}`}>
          {paused ? "PAUSED" : "ACTIVE"}
        </span>
      </div>

      {/* ── Action bar ── */}
      <div className="controls-action-bar">
        <button
          className={`btn-toggle ${paused ? "btn-resume" : "btn-pause"}`}
          onClick={handleToggle}
          disabled={busy === "toggle"}
        >
          {busy === "toggle"
            ? "…"
            : paused
            ? "▶ Resume Bot"
            : "⏸ Pause Bot"}
        </button>

        <button
          className="btn-danger"
          onClick={handleCancelAll}
          disabled={busy === "cancel"}
        >
          {busy === "cancel" ? "Cancelling…" : "✕ Cancel All Orders"}
        </button>

        {cancelResult && (
          <span className={`cancel-result ${cancelResult.startsWith("Error") ? "error" : "ok"}`}>
            {cancelResult}
          </span>
        )}
      </div>

      {/* ── Config sections ── */}
      <div className="controls-body">

        {/* Sizing group */}
        <div className="config-section">
          <div className="config-section-label">
            <span className="config-section-dot" style={{ background: "var(--cyan)" }} />
            Position Sizing
          </div>
          <div className="config-grid">
            <div className="config-group">
              <label className="config-label">Max Bet (USD)</label>
              <input
                type="number"
                className="config-input"
                min="1"
                step="1"
                value={localCfg.max_bet_usd}
                onChange={num("max_bet_usd")}
              />
            </div>

            <div className="config-group">
              <label className="config-label">Max Position / Market</label>
              <input
                type="number"
                className="config-input"
                min="1"
                step="1"
                value={localCfg.max_position_per_market_usd}
                onChange={num("max_position_per_market_usd")}
              />
            </div>

            <div className="config-group">
              <label className="config-label">Max Total Exposure</label>
              <input
                type="number"
                className="config-input"
                min="1"
                step="1"
                value={localCfg.max_total_exposure_usd}
                onChange={num("max_total_exposure_usd")}
              />
            </div>
          </div>
        </div>

        {/* Risk group */}
        <div className="config-section">
          <div className="config-section-label">
            <span className="config-section-dot" style={{ background: "var(--red)" }} />
            Risk Limits
          </div>
          <div className="config-grid">
            <div className="config-group">
              <label className="config-label">Max Daily Loss (USD)</label>
              <input
                type="number"
                className="config-input"
                min="1"
                step="1"
                value={localCfg.max_daily_loss_usd}
                onChange={num("max_daily_loss_usd")}
              />
            </div>

            <div className="config-group">
              <label className="config-label">Max Open Orders</label>
              <input
                type="number"
                className="config-input"
                min="1"
                step="1"
                value={localCfg.max_open_orders}
                onChange={numInt("max_open_orders")}
              />
            </div>
          </div>
        </div>

        {/* Execution group */}
        <div className="config-section">
          <div className="config-section-label">
            <span className="config-section-dot" style={{ background: "var(--purple)" }} />
            Execution
          </div>
          <div className="config-grid config-grid--toggles">
            <div className="config-group config-group--check">
              <label className="config-label">
                <input
                  type="checkbox"
                  checked={localCfg.use_maker_mode}
                  onChange={bool("use_maker_mode")}
                />
                <span>Maker Mode</span>
                <span className="config-hint">GTC orders · 0% fees</span>
              </label>
            </div>

            <div className="config-group config-group--check">
              <label className="config-label">
                <input
                  type="checkbox"
                  checked={localCfg.temporal_arb_enabled}
                  onChange={bool("temporal_arb_enabled")}
                />
                <span>Temporal Arb</span>
                <span className="config-hint">Binance feed</span>
              </label>
            </div>
          </div>
        </div>

        {/* Temporal arb params */}
        {localCfg.temporal_arb_enabled && (
          <div className="config-section config-section--nested">
            <div className="config-section-label">
              <span className="config-section-dot" style={{ background: "var(--yellow)" }} />
              Temporal Arb Parameters
            </div>
            <div className="config-grid">
              <div className="config-group">
                <label className="config-label">Threshold (bps)</label>
                <input
                  type="number"
                  className="config-input"
                  min="1"
                  step="10"
                  value={localCfg.temporal_arb_threshold_bps}
                  onChange={numInt("temporal_arb_threshold_bps")}
                />
              </div>

              <div className="config-group">
                <label className="config-label">Sensitivity (bps)</label>
                <input
                  type="number"
                  className="config-input"
                  min="100"
                  step="100"
                  value={localCfg.temporal_arb_sensitivity_bps}
                  onChange={numInt("temporal_arb_sensitivity_bps")}
                />
              </div>
            </div>
          </div>
        )}

        <div className="config-footer">
          <button
            className="btn-save"
            onClick={handleSaveConfig}
            disabled={busy === "save"}
          >
            {busy === "save" ? "Saving…" : saved ? "✓ Saved" : "Save Config"}
          </button>
          {saved && <span className="save-ok">Changes applied</span>}
        </div>
      </div>

      {/* ── Per-strategy toggles ── */}
      {strategies.length > 0 && (
        <div className="controls-body controls-body--strategies">
          <div className="config-section-label" style={{ marginBottom: 8 }}>
            <span className="config-section-dot" style={{ background: "var(--green)" }} />
            Strategies
          </div>
          <div className="strategy-list">
            {strategies.map((s) => (
              <div key={s.name} className="strategy-row">
                <div className="strategy-row-left">
                  <span className={`strategy-dot ${s.enabled ? "strategy-dot--on" : "strategy-dot--off"}`} />
                  <span className="strategy-name">{s.name}</span>
                </div>
                <div className="strategy-row-right">
                  <span className={`strategy-badge ${s.enabled ? "active" : "paused"}`}>
                    {s.enabled ? "ON" : "OFF"}
                  </span>
                  <button
                    className={`btn-strategy-toggle ${s.enabled ? "btn-pause" : "btn-resume"}`}
                    onClick={() => handleStrategyToggle(s.name, s.enabled)}
                    disabled={strategyBusy === s.name}
                  >
                    {strategyBusy === s.name
                      ? "…"
                      : s.enabled
                      ? "Disable"
                      : "Enable"}
                  </button>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </section>
  );
}
