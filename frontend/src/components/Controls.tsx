import { useState } from "react";
import { api } from "../api";
import type { ControlsInfo } from "../types/api";

interface Props {
  controls: ControlsInfo;
}

export function Controls({ controls }: Props) {
  const [busy, setBusy] = useState<string | null>(null);
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
    <section className="card controls-card">
      {/* ── Bot enable/disable ── */}
      <div className="controls-header">
        <div className="controls-title-row">
          <h2 className="section-title">Bot Controls</h2>
          <span className={`bot-status-badge ${paused ? "paused" : "active"}`}>
            {paused ? "PAUSED" : "ACTIVE"}
          </span>
        </div>

        <div className="controls-actions">
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
        </div>

        {cancelResult && (
          <p className={`cancel-result ${cancelResult.startsWith("Error") ? "error" : "ok"}`}>
            {cancelResult}
          </p>
        )}
      </div>

      {/* ── Strategy config ── */}
      <div className="controls-body">
        <h3 className="controls-section-label">Strategy Parameters</h3>

        <div className="config-grid">
          {/* Sizing */}
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
            <label className="config-label">Max Position / Market (USD)</label>
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
            <label className="config-label">Max Total Exposure (USD)</label>
            <input
              type="number"
              className="config-input"
              min="1"
              step="1"
              value={localCfg.max_total_exposure_usd}
              onChange={num("max_total_exposure_usd")}
            />
          </div>

          {/* Risk */}
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

          {/* Execution */}
          <div className="config-group config-group--check">
            <label className="config-label">
              <input
                type="checkbox"
                checked={localCfg.use_maker_mode}
                onChange={bool("use_maker_mode")}
              />
              <span>Maker Mode (GTC, 0% fees)</span>
            </label>
          </div>

          {/* Temporal arb */}
          <div className="config-group config-group--check">
            <label className="config-label">
              <input
                type="checkbox"
                checked={localCfg.temporal_arb_enabled}
                onChange={bool("temporal_arb_enabled")}
              />
              <span>Temporal Arb (Binance feed)</span>
            </label>
          </div>

          {localCfg.temporal_arb_enabled && (
            <>
              <div className="config-group">
                <label className="config-label">Temporal Threshold (bps)</label>
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
                <label className="config-label">Temporal Sensitivity (bps)</label>
                <input
                  type="number"
                  className="config-input"
                  min="100"
                  step="100"
                  value={localCfg.temporal_arb_sensitivity_bps}
                  onChange={numInt("temporal_arb_sensitivity_bps")}
                />
              </div>
            </>
          )}
        </div>

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
    </section>
  );
}
