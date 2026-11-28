import type { icon } from "../../../../packages/ui/icons.js";

export type Span<T> = { start: T; end: T };
export type ServiceKind = "tcp" | "http" | "https" | "ssh" | "rdp" | "smb";
export type HostOs = "linux" | "windows";
export type DeviceOs = "macos" | "windows" | "android" | "linux";

export type Share = {
  id: string;
  name: string;
  http?: boolean;
  kind?: ServiceKind;
  host_os?: HostOs;
  rules: { ips: Span<string>[]; ports: Span<number>[] };
};

export type Offering = Share & {
  owner: string;
  member: string;
  device?: string;
  device_os?: DeviceOs;
};

export type Label = {
  service: string;
  remark: string;
  alias: string | null;
  ip: string | null;
};

export type Mapping = {
  original: Span<string>;
  virtual_ip: Span<string>;
};

export type LocalMapping = {
  service: string;
  target_ip: string;
  target_port: number;
  local_port: number;
};

export type LocalForwardStatus = LocalMapping & {
  active: boolean;
  error?: string;
  suggested_port?: number;
};

export type DeviceProfile = {
  machine_id: string;
  name: string;
  os: DeviceOs;
  shares: Share[];
  forwards: LocalMapping[];
  updated_at: number;
};

export type ProbeOutcome = {
  kind: ServiceKind;
  rtt_ms: number;
  http_status?: number;
  host_os?: HostOs;
  detail?: string;
};

export type HostPortProbe = {
  port: number;
  open: boolean;
  extra: boolean;
  kind: ServiceKind;
  recommended: boolean;
  rtt_ms?: number;
  http_status?: number;
  detail?: string;
  os_hint?: HostOs;
};

export type HostProbe = {
  ip: string;
  host_os?: HostOs;
  ports: HostPortProbe[];
  duration_ms: number;
  cancelled?: boolean;
};

export type LogEntry = {
  ts: number;
  level: "ERR" | "WARN" | "INFO" | string;
  scope: string;
  message: string;
};

export type FlowKind = "outbound_tun" | "outbound_forward" | "inbound_local" | "inbound_lan";

export type TrafficSample = {
  ts_ms: number;
  tx_bps: number;
  rx_bps: number;
};

export type TrafficFlow = {
  id: number;
  kind: FlowKind;
  service: string;
  service_name: string;
  peer: string;
  source?: string | null;
  target: string;
  local_port?: number | null;
  tx_bytes: number;
  rx_bytes: number;
  tx_bps: number;
  rx_bps: number;
  duration_ms: number;
  anomaly: boolean;
};

export type TrafficSnapshot = {
  enabled: boolean;
  ts_ms: number;
  active_flows: number;
  total_tx_bytes: number;
  total_rx_bytes: number;
  total_tx_bps: number;
  total_rx_bps: number;
  control_tx_bytes: number;
  control_rx_bytes: number;
  history: TrafficSample[];
  flows: TrafficFlow[];
};

export type Status = {
  version?: number;
  server: string;
  member: string | null;
  device_id?: string | null;
  machine_id?: string;
  device_os?: DeviceOs;
  device_name?: string;
  online: boolean;
  running: boolean;
  background: boolean;
  system_routing?: boolean;
  is_admin?: boolean;
  admin_ready?: boolean;
  shares: Share[];
  active: string[];
  active_flows?: number;
  services: Offering[];
  labels: Label[];
  virtual_ips: Mapping[];
  service_virtual_ips?: Record<string, Mapping[]>;
  aliases: Record<string, string>;
  forwards?: LocalForwardStatus[];
  cloud_profiles?: DeviceProfile[];
  restore_prompt?: DeviceProfile | null;
  error: string | null;
  auto_connect?: boolean;
  auto_share?: boolean;
  cloud_backup?: boolean;
  auto_share_blocked?: boolean;
  suggested_port?: number;
  http_status?: number;
  probe?: ProbeOutcome;
  host_probe?: HostProbe;
  admin_data?: unknown;
  logs?: LogEntry[];
  verbose_logs?: boolean;
  traffic?: TrafficSnapshot;
};

export type Request = Record<string, unknown> & { op: string };
export type Reply = { data?: Status; unchanged?: boolean; error?: string };
export type BrandState = "idle" | "connecting" | "active";
export type ViewMode = "grouped" | "flat";
export type CloseBehavior = "tray" | "quit";
export type IconName = Parameters<typeof icon>[0];

export type MenuEntry =
  | { sep: true }
  | { icon: IconName; label: string; danger?: boolean; active?: boolean; onSelect: () => void };

export type Probe = {
  ip: string;
  port: number;
  access: string;
  kind: ServiceKind;
  result?: {
    error?: string;
    httpStatus?: number;
    rttMs?: number;
    hostOs?: HostOs;
    detail?: string;
    visible?: true;
  };
};

export type SourceInfo = {
  text: string;
  self: boolean;
  osType?: DeviceOs;
  memberKey: string;
  memberLabel: string;
  deviceLabel: string;
};

export type UpdateState =
  | { status: "idle" }
  | { status: "checking" }
  | { status: "up_to_date"; current: string; checked_at: number }
  | {
      status: "available";
      current: string;
      version: string;
      notes: string;
      pub_date?: string | null;
      size?: number | null;
      release_url?: string | null;
    }
  | {
      status: "downloading";
      current: string;
      version: string;
      notes: string;
      pub_date?: string | null;
      downloaded: number;
      total?: number | null;
      release_url?: string | null;
    }
  | {
      status: "paused";
      current: string;
      version: string;
      notes: string;
      pub_date?: string | null;
      downloaded: number;
      total?: number | null;
      release_url?: string | null;
    }
  | {
      status: "ready";
      current: string;
      version: string;
      notes: string;
      pub_date?: string | null;
      size: number;
      release_url?: string | null;
    }
  | {
      status: "installing";
      current: string;
      version: string;
      notes: string;
      pub_date?: string | null;
      release_url?: string | null;
    }
  | {
      status: "error";
      current: string;
      stage: string;
      message: string;
      version?: string | null;
      notes?: string | null;
      pub_date?: string | null;
      size?: number | null;
      release_url?: string | null;
    };
