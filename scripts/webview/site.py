"""Opt-in test only. No cache, app bridge, credentials, proxy, or saved page content."""

import json
import os
import sys
from urllib.parse import urlsplit

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("WebKit2", "4.1")
from gi.repository import Gio, GLib, Gtk, WebKit2

URL = os.environ.get("FESEN_LAB_TARGET_URL", "").strip()
if not URL:
    ip = os.environ.get("FESEN_LAB_TARGET_IP", "").strip()
    port = os.environ.get("FESEN_LAB_TARGET_PORT", "").strip()
    if ip and port:
        URL = f"http://{ip}:{port}/"
if not URL:
    print("WebView: missing FESEN_LAB_TARGET_URL (or FESEN_LAB_TARGET_IP/PORT)", flush=True)
    sys.exit(2)
expected = sys.argv[1]
assert expected in ("rendered", "blocked")

context = WebKit2.WebContext.new_ephemeral()
context.get_website_data_manager().set_network_proxy_settings(WebKit2.NetworkProxyMode.NO_PROXY, None)
view = WebKit2.WebView.new_with_context(context)
window = Gtk.Window()
window.set_default_size(412, 800)
window.add(view)
window.show_all()

loop = GLib.MainLoop()
resources = {}
inspecting = False
result = 1
done = False


def finish(ok, message):
    global result, done
    if not done:
        done = True
        result = 0 if ok else 1
        print(message, flush=True)
        loop.quit()
    return False


def resource_started(_view, resource, request):
    uri = request.get_uri()
    complete = True

    def failed_resource(*_):
        nonlocal complete
        complete = False

    def finished(item):
        response = item.get_response()
        if complete and response and urlsplit(response.get_uri())[:2] == urlsplit(URL)[:2]:
            resources[uri] = response.get_status_code()

    resource.connect("failed", failed_resource)
    resource.connect("finished", finished)


def failed(_view, _event, uri, error):
    unreachable = any(error.matches(Gio.io_error_quark(), code) for code in (
        Gio.IOErrorEnum.HOST_UNREACHABLE, Gio.IOErrorEnum.NETWORK_UNREACHABLE))
    ok = expected == "blocked" and uri == URL and unreachable
    finish(ok, "WebView: original address unreachable" if ok else
           f"WebView: unexpected navigation failure ({error.domain}, {error.code})")
    return True  # Don't turn the built-in error page into a successful navigation.


DOM = """
(() => {
  const root = document.querySelector('#root');
  const mounted = !!root && root.childElementCount > 0 && root.getBoundingClientRect().height > 0
    && getComputedStyle(root).visibility !== 'hidden' && root.innerText.trim().length > 0;
  const styles = [...document.querySelectorAll('link[rel~=stylesheet][href]')];
  return JSON.stringify({origin: location.origin, complete: document.readyState === 'complete',
    mounted, applied: styles.every(link => !!link.sheet && !link.sheet.disabled),
    js: [...document.querySelectorAll('script[src]')].map(s => s.src),
    css: styles.map(s => s.href)});
})()
"""


def inspected(page, task):
    global inspecting
    inspecting = False
    if done:
        return
    try:
        dom = json.loads(page.evaluate_javascript_finish(task).to_string())
        if not dom["mounted"] or not dom["complete"] or not dom["applied"]:
            return
        assets = set(dom["js"] + dom["css"])
        if not dom["js"] or not dom["css"] or not all(200 <= resources.get(uri, 0) < 300 for uri in assets):
            return
        if dom["origin"] + "/" != URL:
            return finish(False, "WebView: rendered document left the shared origin")
        finish(True, f"WebView: main HTTP 200; {len(dom['js'])} scripts, {len(dom['css'])} styles; visible #root")
    except GLib.Error:
        finish(False, "WebView: document inspection failed")


def inspect():
    global inspecting
    if done:
        return False
    main = view.get_main_resource()
    response = main.get_response() if main else None
    if response and not view.is_loading() and not inspecting:
        if expected == "blocked":
            return finish(False, "WebView: unexpectedly received a document with sharing off")
        if response.get_status_code() != 200 or response.get_mime_type() != "text/html":
            return finish(False, f"WebView: unexpected main document ({response.get_status_code()})")
        if response.get_uri() != URL or urlsplit(view.get_uri())[:2] != urlsplit(URL)[:2]:
            return finish(False, "WebView: navigation left the shared origin")
        inspecting = True
        view.evaluate_javascript(DOM, -1, None, None, None, inspected)
    return True


view.connect("resource-load-started", resource_started)
view.connect("load-failed", failed)
view.connect("web-process-terminated", lambda *_: finish(False, "WebView: renderer terminated"))
GLib.timeout_add(150, inspect)
GLib.timeout_add_seconds(24, finish, False, "WebView: timed out waiting for navigation, assets or visible content")
view.load_uri(URL)
loop.run()
window.destroy()
sys.exit(result)
