/**
 * Shared zh-CN / en dictionary & locale store used by both Client and Web Admin.
 */
export const SHARED_COPY = {
  brand: ["飞弦", "Fesen"],
  admin_badge: ["管理台", "Admin"],
  admin_title: ["飞弦 · 管理台", "Fesen · Admin"],
  workspace: ["组管理", "Group management"],
  change_password: ["修改密码", "Change password"],
  current_password: ["当前密码", "Current password"],
  confirm_change_password: ["修改后，其他已登录会话与设备须重新登录。", "Other active sessions and devices will need to sign in again."],
  loading: ["加载中…", "Loading…"],
  login_title: ["管理员登录", "Administrator sign-in"],
  account: ["账号", "Username"],
  username: ["账号", "Username"],
  password: ["密码", "Password"],
  new_password: ["新密码", "New password"],
  server: ["服务器", "Server"],
  settings: ["设置", "Settings"],
  appearance: ["外观", "Appearance"],
  theme_system: ["跟随系统", "System"],
  theme_light: ["浅色", "Light"],
  theme_dark: ["深色", "Dark"],
  language: ["语言", "Language"],
  lang_auto: ["跟随系统", "System"],
  lang_zh: ["简体中文", "简体中文"],
  lang_en: ["English", "English"],
  lang_zh_short: ["中文", "中文"],
  lang_en_short: ["EN", "EN"],
  ui_scale: ["元素大小", "Element size"],
  scale_sm: ["小", "Small"],
  scale_md: ["标准", "Standard"],
  scale_lg: ["大", "Large"],
  configure: ["操作", "Actions"],
  more_actions: ["更多", "More"],
  members: ["成员", "Members"],
  devices: ["设备", "Devices"],
  services: ["服务", "Services"],
  devices_empty: ["暂无设备接入", "No devices yet"],
  services_empty: ["暂无广播服务", "No active services"],
  login: ["登录", "Sign in"],
  logging_in: ["登录中…", "Signing in…"],
  cookie_blocked: ["登录未生效，请确认通过正确域名访问且浏览器未禁用 Cookie", "Sign-in did not persist; verify the canonical URL and enable cookies"],
  logout: ["退出登录", "Sign out"],
  join: ["接入链接", "Join link"],
  copy_link: ["复制链接", "Copy link"],
  copied: ["已复制", "Copied"],
  open_join: ["打开飞弦", "Open Fesen"],
  close: ["关闭", "Close"],
  cancel: ["取消", "Cancel"],
  save: ["保存", "Save"],
  done: ["完成", "Done"],
  create: ["新建成员", "Add member"],
  reset: ["重置密码", "Reset password"],
  refresh: ["刷新", "Refresh"],
  admin: ["管理员", "Admin"],
  disabled: ["已停用", "Disabled"],
  enabled: ["正常", "Active"],
  enable: ["启用", "Enable"],
  disable: ["停用", "Disable"],
  revoked: ["已撤销", "Revoked"],
  online_state: ["在线", "Online"],
  offline_state: ["离线", "Offline"],
  broadcasting: ["广播中", "Live"],
  source_self: ["本账号", "My account"],
  badge_local: ["本机", "Local"],
  revoke: ["撤销设备", "Revoke device"],
  remove_device: ["移除记录", "Remove record"],
  confirm_disable: ["停用后，该账号下所有设备将立即断开。", "All devices on this account will disconnect."],
  confirm_revoke: ["撤销后，该设备将立即断开并作废当前凭据；重新接入须重新验证账号密码。", "This device will disconnect immediately and its credentials will be invalidated. Reconnecting requires password sign-in."],
  confirm_remove_device: ["将从设备列表移除该已撤销的历史记录。", "Remove this revoked device record from the list."],
  confirm_reset: ["重置后，该账号下所有设备须重新登录。", "All devices on this account will need to sign in again."],
  device_revoked: ["此设备已被管理员撤销，请重新验证密码登录", "This device was revoked. Sign in again with your password."],
  unauthorized: ["未授权或账号密码错误", "Unauthorized or incorrect credentials"],
  conflict: ["地址或账号冲突", "Conflict"],
  invalid: ["输入无效", "Invalid input"],
  busy: ["当前忙，请稍后再试", "Busy, try again shortly"],
  network: ["无法连接服务器", "Cannot reach server"],
  not_connected: ["未连接", "Not connected"],
  internal: ["操作失败", "Action failed"],
  ipv4: ["IPv4 地址", "IPv4 address"],
  port: ["端口", "Port"],
};

/**
 * @param {string} [storageKey]
 * @param {Record<string, readonly [string, string]>} [extra]
 */
export function createI18n(storageKey = "fesen_locale", extra = {}) {
  const dict = { ...SHARED_COPY, ...extra };
  const listeners = new Set();

  function detectSystem() {
    const lang = (navigator.languages || [navigator.language]).find((v) => /^(zh|en)(-|$)/i.test(v || ""));
    return /^zh(-|$)/i.test(lang || "") ? "zh-CN" : "en";
  }

  function readMode() {
    try {
      const saved = localStorage.getItem(storageKey);
      if (saved === "zh-CN" || saved === "en" || saved === "auto") return saved;
    } catch {}
    return "auto";
  }

  let mode = readMode();
  let locale = mode === "auto" ? detectSystem() : mode;
  document.documentElement.lang = locale;

  const getMode = () => mode;
  const getLocale = () => locale;
  const isZh = () => locale === "zh-CN";
  const setLocale = (next) => {
    mode = next === "en" || next === "zh-CN" ? next : "auto";
    try { localStorage.setItem(storageKey, mode); } catch {}
    locale = mode === "auto" ? detectSystem() : mode;
    document.documentElement.lang = locale;
  };
  const onChange = (fn) => {
    listeners.add(fn);
    return () => listeners.delete(fn);
  };
  window.addEventListener("languagechange", () => {
    if (mode !== "auto") return;
    const next = detectSystem();
    if (next === locale) return;
    locale = next;
    document.documentElement.lang = locale;
    for (const fn of listeners) fn();
  });
  const t = (key) => {
    const entry = Object.hasOwn(dict, key) ? dict[key] : undefined;
    return entry ? entry[isZh() ? 0 : 1] : key;
  };
  const failure = (code) => {
    if (typeof code === "string" && code.startsWith("HTTP ")) return code;
    if (typeof code === "string" && Object.hasOwn(dict, code)) return t(code);
    return t("internal");
  };

  return { getMode, getLocale, setLocale, onChange, isZh, t, failure };
}
