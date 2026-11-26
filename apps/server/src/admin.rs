use crate::{
    group::{App, Group},
    store,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, OriginalUri, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use fesen_proto::Code;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

fn asset_etag(body: &'static str) -> HeaderValue {
    let digest = Sha256::digest(body.as_bytes());
    let tag = format!("\"{}\"", hex::encode(&digest[..8]));
    HeaderValue::from_str(&tag).unwrap()
}

fn static_asset(
    content_type: &'static str,
    body: &'static str,
    etag: &'static OnceLock<HeaderValue>,
    headers: &HeaderMap,
) -> Response {
    let etag = etag.get_or_init(|| asset_etag(body));
    let matched = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|raw| {
            let expected = etag.to_str().unwrap_or("");
            raw.split(',')
                .map(str::trim)
                .any(|cand| cand == "*" || cand == expected || cand.strip_prefix("W/") == Some(expected))
        });
    if matched {
        return (
            StatusCode::NOT_MODIFIED,
            [
                (header::ETAG, etag.clone()),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            ],
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::ETAG, etag.clone()),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

fn js_route(body: &'static str, etag: &'static OnceLock<HeaderValue>) -> axum::routing::MethodRouter<App> {
    get(move |headers: HeaderMap| async move {
        static_asset("text/javascript; charset=utf-8", body, etag, &headers)
    })
}

fn canonical_entry(uri: &Uri, headers: &HeaderMap, app: &App) -> Option<Response> {
    let host = uri
        .authority()
        .map(|a| a.as_str())
        .or_else(|| headers.get(header::HOST).and_then(|v| v.to_str().ok()))?;
    if app.expected_authority.is_empty()
        || host.eq_ignore_ascii_case(app.expected_authority.as_ref())
    {
        return None;
    }
    let target = app.origin.join("/admin/").ok()?;
    Some(axum::response::Redirect::temporary(target.as_str()).into_response())
}

static ETAG_APP_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_ICONS_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_THEME_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_I18N_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_POPOVER_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_HOST_CARD_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_SETTINGS_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_ADMIN_VIEW_JS: OnceLock<HeaderValue> = OnceLock::new();
static ETAG_STYLE_CSS: OnceLock<HeaderValue> = OnceLock::new();

pub fn router() -> Router<App> {
    Router::new()
        .route(
            "/",
            get(
                |State(app): State<App>, OriginalUri(uri): OriginalUri, headers: HeaderMap| async move {
                    if let Some(redirect) = canonical_entry(&uri, &headers, &app) {
                        return redirect;
                    }
                    Html(include_str!("../web/index.html")).into_response()
                },
            ),
        )
        .route("/app.js", js_route(include_str!("../web/app.js"), &ETAG_APP_JS))
        .route("/icons.js", js_route(include_str!("../../../packages/ui/icons.js"), &ETAG_ICONS_JS))
        .route("/theme.js", js_route(include_str!("../../../packages/ui/theme.js"), &ETAG_THEME_JS))
        .route("/i18n.js", js_route(include_str!("../../../packages/ui/i18n.js"), &ETAG_I18N_JS))
        .route("/popover.js", js_route(include_str!("../../../packages/ui/popover.js"), &ETAG_POPOVER_JS))
        .route("/host-card.js", js_route(include_str!("../../../packages/ui/host-card.js"), &ETAG_HOST_CARD_JS))
        .route("/settings.js", js_route(include_str!("../../../packages/ui/settings.js"), &ETAG_SETTINGS_JS))
        .route("/admin-view.js", js_route(include_str!("../../../packages/ui/admin-view.js"), &ETAG_ADMIN_VIEW_JS))
        .route(
            "/style.css",
            get(|headers: HeaderMap| async move {
                static_asset(
                    "text/css; charset=utf-8",
                    concat!(
                        include_str!("../../../packages/ui/styles/tokens.css"),
                        "\n",
                        include_str!("../../../packages/ui/styles/layout.css"),
                        "\n",
                        include_str!("../../../packages/ui/styles/controls.css"),
                        "\n",
                        include_str!("../../../packages/ui/styles/surfaces.css"),
                        "\n",
                        include_str!("../../../packages/ui/styles/responsive.css"),
                        "\n",
                        include_str!("../web/style.css")
                    ),
                    &ETAG_STYLE_CSS,
                    &headers,
                )
            }),
        )
        .route("/login", post(login))
        .route("/session", get(session))
        .route("/action", post(action))
        .layer(DefaultBodyLimit::max(4096))
        .layer(middleware::from_fn(headers))
}

async fn headers(req: axum::extract::Request, next: Next) -> Response {
    let mut r = next.run(req).await;
    let headers = r.headers_mut();
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert_with(|| HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
    );
    r
}

struct Failure(Code);
impl From<Code> for Failure {
    fn from(code: Code) -> Self {
        Self(code)
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let status = match self.0 {
            Code::Unauthorized => StatusCode::UNAUTHORIZED,
            Code::Conflict => StatusCode::CONFLICT,
            Code::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Code::Busy => StatusCode::TOO_MANY_REQUESTS,
            _ => StatusCode::BAD_REQUEST,
        };
        (status, Json(json!({"error": self.0}))).into_response()
    }
}

fn check(headers: &HeaderMap, app: &App) -> Result<(), Code> {
    if headers.get("x-fesen-admin").is_none_or(|h| h != "1")
        || headers
            .get(header::ORIGIN)
            .is_some_and(|h| h != app.origin_header.as_ref())
    {
        return Err(Code::Unauthorized);
    }
    Ok(())
}

fn get_tokens(headers: &HeaderMap) -> impl Iterator<Item = &str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .filter_map(|c| c.trim().strip_prefix("fesen_admin="))
        .filter(|t| t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn gate(group: &Group, headers: &HeaderMap) -> Result<String, Code> {
    get_tokens(headers)
        .find_map(|tok| store::admin(&group.db, tok))
        .ok_or(Code::Unauthorized)
}

fn cookie(app: &App, token: &str, max_age: u32) -> HeaderValue {
    let secure = if app.origin.scheme() == "https" {
        "; Secure"
    } else {
        ""
    };
    HeaderValue::from_str(&format!(
        "fesen_admin={token}; Path=/admin; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}"
    ))
    .unwrap()
}

#[derive(Deserialize)]
struct Login {
    username: String,
    password: String,
}

async fn login(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Login>,
) -> Result<Response, Failure> {
    check(&headers, &app)?;
    let hash = app.verify(&input.username, input.password, true).await?;
    let g = app.inner.lock().unwrap();
    if store::login_hash(&g.db, &input.username, true).as_ref() != Some(&hash) {
        return Err(Code::Unauthorized.into());
    }
    let token = store::secret();
    store::admin_cookie(&g.db, &input.username, &token)?;
    Ok((
        [(header::SET_COOKIE, cookie(&app, &token, 43200))],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

async fn session(State(app): State<App>, headers: HeaderMap) -> Result<Response, Failure> {
    let g = app.inner.lock().unwrap();
    let admin = gate(&g, &headers)?;
    let (users, mut devices) = store::list(&g.db)?;
    for d in &mut devices {
        d.online = g.online.contains_key(&d.id);
    }
    let services = g.services();
    drop(g);
    Ok(
        Json(json!({
            "admin": admin,
            "users": users,
            "devices": devices,
            "link": app.join_link.as_ref(),
            "services": services,
        }))
        .into_response(),
    )
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Action {
    Create {
        username: String,
        password: String,
        #[serde(default)]
        admin: bool,
    },
    Reset { username: String, password: String },
    Enable { username: String, enabled: bool },
    SetAdmin { username: String, admin: bool },
    Revoke { device: String },
    Logout,
}

async fn action(
    State(app): State<App>,
    headers: HeaderMap,
    Json(action): Json<Action>,
) -> Result<Response, Failure> {
    check(&headers, &app)?;
    let changed = match action {
        Action::Create {
            username,
            password,
            admin,
        } => {
            gate(&app.inner.lock().unwrap(), &headers)?;
            let hash = app.hash(password).await?;
            let g = app.inner.lock().unwrap();
            gate(&g, &headers)?;
            store::create(&g.db, &username, &hash, admin)?;
            false
        }
        Action::Reset { username, password } => {
            gate(&app.inner.lock().unwrap(), &headers)?;
            let hash = app.hash(password).await?;
            let mut g = app.inner.lock().unwrap();
            gate(&g, &headers)?;
            store::change_user(&mut g.db, &username, Some(&hash), None)?;
            g.remove_member(&username);
            true
        }
        Action::Enable { username, enabled } => {
            let mut g = app.inner.lock().unwrap();
            gate(&g, &headers)?;
            store::change_user(&mut g.db, &username, None, Some(!enabled))?;
            g.remove_member(&username);
            true
        }
        Action::SetAdmin { username, admin } => {
            let mut g = app.inner.lock().unwrap();
            let actor = gate(&g, &headers)?;
            store::set_admin(&mut g.db, &actor, &username, admin)?;
            false
        }
        Action::Revoke { device } => {
            let mut g = app.inner.lock().unwrap();
            gate(&g, &headers)?;
            store::revoke(&g.db, &device)?;
            g.remove(&device, true);
            true
        }
        Action::Logout => {
            let g = app.inner.lock().unwrap();
            gate(&g, &headers)?;
            for tok in get_tokens(&headers) {
                g.db.execute(
                    "DELETE FROM admin_sessions WHERE token=?1",
                    [store::hash_secret(tok)],
                )
                .map_err(|_| Code::Internal)?;
            }
            return Ok((
                [(header::SET_COOKIE, cookie(&app, "", 0))],
                StatusCode::NO_CONTENT,
            )
                .into_response());
        }
    };
    if changed {
        app.changed();
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
