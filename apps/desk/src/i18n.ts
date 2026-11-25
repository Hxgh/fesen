export type Locale = "zh-CN" | "en";

const copy = {
  brand: ["飞弦", "Fesen"],
  offline: ["设备离线", "Device offline"],
  direct_failed: ["无法直连", "Direct failed"],
  conflict: ["地址冲突", "Conflict"],
  unauthorized: ["未授权", "Unauthorized"],
  login_rejected: ["账号或密码错误，或设备不可用", "Incorrect credentials or device unavailable"],
  target_timeout: ["目标超时", "Timed out"],
  target_unavailable: ["目标不可达", "Unreachable"],
  publisher_target_unavailable: ["共享进程连不上目标，请检查地址与「本地网络」权限。", "Publisher cannot reach target. Check address and Local Network permission."],
  http_invalid: ["HTTP 响应无效", "Invalid HTTP"],
  protocol_mismatch: ["协议不匹配", "Protocol mismatch"],
  invalid: ["输入无效", "Invalid input"],
  limit: ["已达到容量上限", "Capacity reached"],
  network_changed: ["网络已变更，共享已暂停", "Network changed; sharing paused"],
  not_connected: ["未连接", "Not connected"],
  busy: ["当前忙", "Busy"],
  internal: ["操作失败", "Action failed"],
  vpn_permission: ["需要允许 VPN 接入", "Allow VPN access"],
  extension_permission: ["请在系统设置中允许飞弦网络扩展，然后返回继续。", "Allow the Fesen network extension in System Settings, then return to continue."],
  extension_unsigned: ["此安装包未含有效网络扩展签名，仅支持发布。", "Unsigned build without network extension entitlement."],
  extension_failed: ["无法启用网络扩展，请重试。", "Could not enable the network extension."],
  extension_location: ["请将飞弦移至「应用程序」后重新打开。", "Move Fesen to Applications and reopen."],
  extension_restart: ["网络扩展需重启 Mac 后生效。", "Restart your Mac to finish enabling the extension."],
  enable_extension: ["启用网络扩展", "Enable extension"],
  system_settings: ["打开系统设置", "System Settings"],
  settings: ["设置", "Settings"],
  appearance: ["外观", "Appearance"],
  theme_system: ["跟随系统", "System"],
  theme_light: ["浅色", "Light"],
  theme_dark: ["深色", "Dark"],
  language: ["语言", "Language"],
  lang_zh: ["简体中文", "简体中文"],
  lang_en: ["English", "English"],
  on_window_close: ["关闭窗口时", "On window close"],
  close_to_tray: ["留驻菜单栏", "Keep in menu bar"],
  close_quit: ["彻底退出", "Quit completely"],
  close_to_tray_desc: ["退出界面进程，保留菜单栏与后台核心。", "Exits UI; keeps menu bar and core running."],
  close_quit_desc: ["停止所有共享与连接并退出程序。", "Stops all shares and exits all processes."],
  menu_save_failed: ["菜单栏设置未保存，请重试。", "Could not save menu bar preference."],
  retry: ["重新加载", "Reload"],
  expand_window: ["放大窗口", "Expand"],
  restore_window: ["还原大小", "Restore"],
  continue: ["继续", "Continue"],
  cancel: ["取消", "Cancel"],
  save: ["保存", "Save"],
  done: ["完成", "Done"],
  access_settings: ["访问设置", "Access settings"],
  more_actions: ["更多", "More"],
  copy_address: ["复制地址", "Copy address"],
  copy_command: ["复制连接命令", "Copy command"],
  copy_short: ["复制", "Copy"],
  address_copied: ["已复制", "Copied"],
  open_browser: ["浏览器打开", "Open in browser"],
  open_web: ["打开", "Open"],
  web_test: ["网页预览", "Web preview"],
  local_web_test: ["本机预览", "Local preview"],
  clipboard_failed: ["无法复制", "Copy failed"],
  external_browser_failed: ["无法打开浏览器", "Could not open browser"],
  close: ["关闭", "Close"],
  connection: ["连接", "Connection"],
  server: ["服务器", "Server"],
  stop_edit: ["停止并编辑", "Stop & edit"],
  remark: ["备注", "Display name"],
  alias: ["访问别名", "Alias"],
  alias_ip: ["原 IPv4", "Original IPv4"],
  test_connection: ["测试连接", "Test"],
  test_host: ["检测全部端口", "Test host"],
  reprobe: ["点击检测", "Click to test"],
  invalid_target: ["目标不在该共享范围内", "Target outside this share"],
  browser_failed: ["无法打开预览窗口", "Could not open preview"],
  page_loading: ["加载中…", "Loading…"],
  page_visible: ["正常", "OK"],
  page_empty: ["目标未返回内容", "Empty response"],
  page_not_rendered: ["未渲染出内容", "Nothing rendered"],
  route_missing: ["路由尚未就绪", "Route not ready"],
  test: ["测试", "Test"],
  original_ip: ["原 IP", "Original"],
  port: ["端口", "Port"],
  no_services: ["组内暂无共享服务", "No shared services in group"],
  group_disconnected: ["未接入组", "Disconnected"],
  refresh_status: ["检测状态", "Check status"],
  unchecked: ["未测", "Test"],
  available: ["正常", "OK"],
  unavailable: ["不可达", "Failed"],
  overlap: ["冲突", "Conflict"],
  local_overlap: ["本地网段重叠，已映射虚拟地址", "Local overlap; mapped to virtual IP"],
  testing: ["…", "…"],
  reachable: ["正常", "OK"],
  edit_share: ["编辑共享", "Edit share"],
  new_share: ["新增共享", "New share"],
  mode_host: ["主机", "Host"],
  mode_custom: ["自定义规则", "Custom"],
  host_ip: ["IPv4 地址", "IPv4 address"],
  host_name: ["名称（选填）", "Name (optional)"],
  add_ports_label: ["端口", "Ports"],
  add_ports_placeholder: ["输入端口回车添加，支持逗号分隔", "Enter port(s), comma or Enter to add"],
  add_more_ports_placeholder: ["+ 追加其他端口，回车添加", "+ Add another port, press Enter"],
  detect_host: ["检测服务", "Check services"],
  detecting_host: ["检测中…", "Checking…"],
  manage_host_ports: ["管理端口", "Manage ports"],
  host_none_open: ["未检测到开放的常用端口", "No common ports open"],
  port_offline: ["未响应", "No response"],
  port_checking: ["检测中", "Checking"],
  already_added: ["已有", "Saved"],
  save_and_publish: ["立即共享", "Share immediately"],
  select_one_service: ["请至少保留一个端口", "Select at least one port"],
  invalid_host_ip: ["请输入有效的单 IPv4 地址", "Enter a valid non-reserved IPv4"],
  invalid_port_input: ["端口无效（1–65535，单主机最多 10 个）", "Invalid port (1–65535, up to 10)"],
  protocol_type: ["协议", "Protocol"],
  kind_tcp: ["TCP", "TCP"],
  kind_http: ["HTTP", "HTTP"],
  kind_https: ["HTTPS", "HTTPS"],
  kind_ssh: ["SSH", "SSH"],
  kind_rdp: ["RDP", "RDP"],
  kind_smb: ["SMB", "SMB"],
  service_name: ["名称", "Name"],
  ipv4: ["IPv4 地址", "IPv4 address"],
  publish: ["共享", "Share"],
  published: ["共享中", "Sharing"],
  unpublished: ["未共享", "Off"],
  remove: ["删除", "Delete"],
  confirm_remove: ["删除此共享？", "Delete this share?"],
  no_presets: ["暂无共享", "No shares yet"],
  open: ["打开飞弦", "Open Fesen"],
  join_group: ["接入组", "Join group"],
  join_link: ["接入链接", "Join link"],
  change: ["更换", "Change"],
  username: ["账号", "Username"],
  password: ["密码", "Password"],
  device: ["设备名", "Device"],
  login: ["登录", "Sign in"],
  online: ["已连接 · 点击断开", "Connected · Click to disconnect"],
  connecting: ["连接中 · 点击取消", "Connecting · Click to cancel"],
  disconnected: ["点击连接", "Click to connect"],
  routing_unavailable: ["当前安装包未含系统路由扩展，仅支持对外共享。", "Unsigned build without system routing; publishing only."],
  stop: ["断开", "Disconnect"],
  start: ["接入", "Connect"],
  logout: ["退出登录", "Sign out"],
  services: ["服务", "Services"],
  group: ["组内服务", "Group"],
  mine: ["我的共享", "My shares"],
} as const;

export type Key = keyof typeof copy;

function detectInitialLocale(): Locale {
  try {
    const saved = localStorage.getItem("fesen_locale");
    if (saved === "zh-CN" || saved === "en") return saved;
  } catch {}
  const language = navigator.languages.find((value) => /^(zh|en)(-|$)/i.test(value));
  return /^zh(-|$)/i.test(language ?? "") ? "zh-CN" : "en";
}

let activeLocale: Locale = detectInitialLocale();

export function getLocale(): Locale {
  return activeLocale;
}

export function setLocale(loc: Locale): void {
  activeLocale = loc;
  try {
    localStorage.setItem("fesen_locale", loc);
  } catch {}
  document.documentElement.lang = loc;
}

export function isZh(): boolean {
  return activeLocale === "zh-CN";
}

export const t = (key: Key): string => {
  const item = copy[key];
  if (!item) return key;
  return item[isZh() ? 0 : 1];
};

export const failure = (code: string): string => {
  if (code.startsWith("HTTP ")) return code;
  if (code in copy) return t(code as Key);
  return t("internal");
};
