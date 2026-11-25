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

fn js_asset(body: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], body)
}

fn canonical_entry(uri: &Uri, headers: &HeaderMap, app: &App) -> Option<Response> {
    let host = uri
        .authority()
        .map(|a| a.as_str())
        .or_else(|| headers.get(header::HOST).and_then(|v| v.to_str().ok()))?;
    let expected_host = app.origin.host_str()?;
    let expected_authority = match app.origin.port() {
        Some(p) => format!("{expected_host}:{p}"),
        None => expected_host.to_owned(),
    };
    if host.eq_ignore_ascii_case(&expected_authority) {
        return None;
    }
    let target = app.origin.join("/admin/").ok()?;
    Some(axum::response::Redirect::temporary(target.as_str()).into_response())
}

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
        .route("/app.js", get(|| async { js_asset(include_str!("../web/app.js")) }))
        .route("/icons.js", get(|| async { js_asset(include_str!("../../../packages/ui/icons.js")) }))
        .route("/theme.js", get(|| async { js_asset(include_str!("../../../packages/ui/theme.js")) }))
        .route("/i18n.js", get(|| async { js_asset(include_str!("../../../packages/ui/i18n.js")) }))
        .route("/popover.js", get(|| async { js_asset(include_str!("../../../packages/ui/popover.js")) }))
        .route("/host-card.js", get(|| async { js_asset(include_str!("../../../packages/ui/host-card.js")) }))
        .route("/settings.js", get(|| async { js_asset(include_str!("../../../packages/ui/settings.js")) }))
        .route("/admin-view.js", get(|| async { js_asset(include_str!("../../../packages/ui/admin-view.js")) }))
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    concat!(
                        include_str!("../../../packages/ui/base.css"),
                        "\n",
                        include_str!("../web/style.css")
                    ),
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
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
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
            .is_some_and(|h| h != app.origin.origin().ascii_serialization().as_str())
    {
        return Err(Code::Unauthorized);
    }
    Ok(())
}

fn get_token(headers: &HeaderMap) -> Result<&str, Code> {
    headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|cookies| {
            cookies
                .split(';')
                .find_map(|c| c.trim().strip_prefix("fesen_admin="))
        })
        .filter(|t| t.len() == 64)
        .ok_or(Code::Unauthorized)
}

fn gate(group: &Group, headers: &HeaderMap) -> Result<String, Code> {
    store::admin(&group.db, get_token(headers)?).ok_or(Code::Unauthorized)
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
    let mut link = url::Url::parse("fesen://join").unwrap();
    link.query_pairs_mut()
        .append_pair("server", app.origin.as_str());
    let view = g.view();
    Ok(
        Json(json!({
            "admin": admin,
            "users": users,
            "devices": devices,
            "link": link.as_str(),
            "services": view.services,
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
    gate(&app.inner.lock().unwrap(), &headers)?;
    let changed = match action {
        Action::Create {
            username,
            password,
            admin,
        } => {
            let hash = app.hash(password).await?;
            let g = app.inner.lock().unwrap();
            gate(&g, &headers)?;
            store::create(&g.db, &username, &hash, admin)?;
            false
        }
        Action::Reset { username, password } => {
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
            g.db.execute(
                "DELETE FROM admin_sessions WHERE token=?1",
                [store::hash_secret(get_token(&headers)?)],
            )
            .map_err(|_| Code::Internal)?;
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
