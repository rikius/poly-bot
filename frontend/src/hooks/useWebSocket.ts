import { useEffect, useRef, useState, useCallback } from "react";
import type { WsSnapshot } from "../types/api";

type ConnectionStatus = "connecting" | "connected" | "disconnected" | "error";

const WS_URL =
  typeof window !== "undefined"
    ? `${window.location.protocol === "https:" ? "wss" : "ws"}://${window.location.host}/ws`
    : "ws://localhost:3001/ws";

const RECONNECT_DELAY_MS = 2000;
const MAX_RECONNECT_DELAY_MS = 30000;

export function useWebSocket() {
  const [snapshot, setSnapshot] = useState<WsSnapshot | null>(null);
  const [status, setStatus] = useState<ConnectionStatus>("connecting");
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);

  const wsRef = useRef<WebSocket | null>(null);
  const reconnectDelayRef = useRef(RECONNECT_DELAY_MS);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mountedRef = useRef(true);

  const connect = useCallback(() => {
    if (!mountedRef.current) return;

    setStatus("connecting");

    const ws = new WebSocket(WS_URL);
    wsRef.current = ws;

    ws.onopen = () => {
      if (!mountedRef.current) return;
      setStatus("connected");
      reconnectDelayRef.current = RECONNECT_DELAY_MS;
    };

    ws.onmessage = (event: MessageEvent<string>) => {
      if (!mountedRef.current) return;
      try {
        const data = JSON.parse(event.data) as WsSnapshot;
        if (data.type === "snapshot") {
          setSnapshot(data);
          setLastUpdated(new Date());
        }
      } catch {
        // ignore malformed messages
      }
    };

    ws.onerror = () => {
      if (!mountedRef.current) return;
      setStatus("error");
    };

    ws.onclose = () => {
      if (!mountedRef.current) return;
      setStatus("disconnected");
      wsRef.current = null;

      // Exponential backoff reconnect
      const delay = reconnectDelayRef.current;
      reconnectDelayRef.current = Math.min(
        delay * 2,
        MAX_RECONNECT_DELAY_MS,
      );
      reconnectTimerRef.current = setTimeout(connect, delay);
    };
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    connect();

    return () => {
      mountedRef.current = false;
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current);
      }
      wsRef.current?.close();
    };
  }, [connect]);

  return { snapshot, status, lastUpdated };
}
