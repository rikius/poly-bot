/** Thin wrappers around the bot's REST control endpoints. */

const BASE = "";

async function post(path: string): Promise<{ ok: boolean; [k: string]: unknown }> {
  const res = await fetch(`${BASE}${path}`, { method: "POST" });
  return res.json();
}

async function patchJson(
  path: string,
  body: Record<string, unknown>
): Promise<{ ok: boolean; [k: string]: unknown }> {
  const res = await fetch(`${BASE}${path}`, {
    method: "PATCH",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  return res.json();
}

export const api = {
  pauseBot: () => post("/api/bot/pause"),
  resumeBot: () => post("/api/bot/resume"),
  cancelAllOrders: () => post("/api/orders/cancel-all"),
  patchConfig: (fields: Record<string, unknown>) => patchJson("/api/config", fields),
  enableStrategy: (name: string) => post(`/api/strategies/${encodeURIComponent(name)}/enable`),
  disableStrategy: (name: string) => post(`/api/strategies/${encodeURIComponent(name)}/disable`),
};
