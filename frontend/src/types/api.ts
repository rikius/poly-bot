export interface BotStatus {
  mode: "paper" | "live";
  is_live: boolean;
  uptime_secs: number;
}

export interface CashInfo {
  available: string;
  reserved: string;
  total: string;
}

export interface PositionInfo {
  token_id: string;
  shares: string;
  avg_cost: string;
  cost_basis: string;
  realized_pnl: string;
  unrealized_pnl: string;
  total_pnl: string;
  total_fees: string;
  direction: "long" | "short";
  notional: string;
}

export interface OrderInfo {
  local_id: string;
  order_id: string | null;
  token_id: string;
  side: "buy" | "sell";
  price: string;
  original_size: string;
  filled_size: string;
  remaining_size: string;
  state: string;
  strategy_id: string | null;
  created_at: string;
}

export interface FillInfo {
  fill_id: string;
  order_id: string;
  token_id: string;
  side: "buy" | "sell";
  price: string;
  size: string;
  fee: string;
  notional: string;
  timestamp: string;
}

export interface PnlInfo {
  realized: string;
  unrealized: string;
  total: string;
  total_fees: string;
  net: string;
}

export interface OrderStatsInfo {
  total_created: number;
  total_filled: number;
  total_cancelled: number;
  total_rejected: number;
  active_count: number;
}

export interface WsSnapshot {
  type: "snapshot";
  timestamp: string;
  bot_status: BotStatus;
  cash: CashInfo;
  positions: PositionInfo[];
  active_orders: OrderInfo[];
  order_stats: OrderStatsInfo;
  recent_fills: FillInfo[];
  pnl: PnlInfo;
}
