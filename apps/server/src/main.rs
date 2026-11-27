mod admin;
mod control;
mod group;
mod store;
#[cfg(test)]
mod tests;
mod tls;

use axum::{Router, routing::get};
use fesen_proto::{origin, stun_endpoint, valid_username};
use group::App;
use std::{
    env,
    net::{SocketAddr, SocketAddrV4},
    path::{Path, PathBuf},
};
use tokio::net::{TcpListener, UdpSocket};

struct Discovery {
    listen: Option<SocketAddrV4>,
    public_port: Option<u16>,
    stun: Option<String>,
}

impl Discovery {
    fn parse(listen: &str, public_port: &str, stun: &str) -> anyhow::Result<Self> {
        let listen = (!listen.is_empty()).then(|| listen.parse()).transpose()?;
        let public_port = (!public_port.is_empty())
            .then(|| public_port.parse::<u16>())
            .transpose()?;
        anyhow::ensure!(
            public_port.is_none() || (listen.is_some() && public_port != Some(0)),
            "FESEN_PUBLIC_UDP_PORT 需要内置 UDP 监听和非零端口"
        );
        let stun = (!stun.is_empty()).then(|| stun.to_owned());
        if let Some(stun) = &stun {
            anyhow::ensure!(
                stun_endpoint(stun).is_some(),
                "FESEN_STUN 应为域名或 IPv4:端口"
            );
            anyhow::ensure!(listen.is_none(), "内置 UDP 探测与 STUN 只能选一种");
        }
        Ok(Self {
            listen,
            public_port,
            stun,
        })
    }
}

fn read_password(prompt: &str) -> anyhow::Result<String> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        Ok(rpassword::prompt_password(prompt)?)
    } else {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        Ok(s.trim_end_matches(['\r', '\n']).to_owned())
    }
}

fn bootstrap_admin(
    db: &rusqlite::Connection,
    user_env: Option<&str>,
    pass_env: Option<&str>,
    pass_file_env: Option<&Path>,
) -> anyhow::Result<()> {
    let pass_env = pass_env.filter(|s| !s.is_empty());
    let pass_file_env = pass_file_env.filter(|p| !p.as_os_str().is_empty());
    anyhow::ensure!(
        !(pass_env.is_some() && pass_file_env.is_some()),
        "FESEN_ADMIN_PASSWORD 与 FESEN_ADMIN_PASSWORD_FILE 只能设置一种"
    );
    let has_bootstrap = pass_env.is_some() || pass_file_env.is_some();
    if store::has_admin(db)? {
        if has_bootstrap {
            println!("提示: 超级管理员已存在，已忽略初始密码环境变量，建议从部署配置中移除");
        }
        return Ok(());
    }
    let password = match (pass_env, pass_file_env) {
        (Some(p), None) => p.to_owned(),
        (None, Some(file)) => std::fs::read_to_string(file)
            .map_err(|e| anyhow::anyhow!("无法读取 FESEN_ADMIN_PASSWORD_FILE: {e}"))?
            .trim_end_matches(['\r', '\n'])
            .to_owned(),
        _ => {
            println!(
                "提示: 尚未创建超级管理员，可设置 FESEN_ADMIN_PASSWORD 启动，或运行 fesen-server admin <账号>"
            );
            return Ok(());
        }
    };
    let user = user_env
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("admin");
    anyhow::ensure!(valid_username(user), "FESEN_ADMIN_USER 账号无效");
    let hash = store::password_hash(&password)
        .map_err(|_| anyhow::anyhow!("初始管理员密码无效（长度需 8-256 位）"))?;
    store::create(db, user, &hash, true)?;
    println!("已初始化超级管理员: {user}");
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = PathBuf::from(env::var_os("FESEN_DB").unwrap_or_else(|| "fesen.sqlite".into()));
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("admin") => {
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("用法: fesen-server admin 账号"))?;
            anyhow::ensure!(args.next().is_none() && valid_username(&name), "账号无效");
            let db = store::db(&path)?;
            anyhow::ensure!(
                !store::has_admin(&db)?,
                "已存在超级管理员；如需重置密码请运行 fesen-server passwd <账号>"
            );
            let password = read_password("超级管理员密码: ")?;
            let hash = store::password_hash(&password)?;
            store::create(&db, &name, &hash, true)?;
            println!("超级管理员账号已创建");
            return Ok(());
        }
        Some("passwd") => {
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("用法: fesen-server passwd 账号"))?;
            anyhow::ensure!(args.next().is_none() && valid_username(&name), "账号无效");
            let mut db = store::db(&path)?;
            let password = read_password("新密码: ")?;
            let hash = store::password_hash(&password)?;
            store::reset_password(&mut db, &name, &hash)
                .map_err(|_| anyhow::anyhow!("账号不存在或已停用"))?;
            println!("密码已重置，旧会话与设备令牌已失效");
            return Ok(());
        }
        Some("serve") | None => anyhow::ensure!(
            args.next().is_none(),
            "用法: fesen-server [serve | admin 账号 | passwd 账号]"
        ),
        _ => anyhow::bail!("用法: fesen-server [serve | admin 账号 | passwd 账号]"),
    }
    let origin =
        origin(&env::var("FESEN_ORIGIN").unwrap_or_else(|_| "http://127.0.0.1:8080".into()))?;
    let listen: SocketAddr = env::var("FESEN_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:8080".into())
        .parse()?;
    let config = Discovery::parse(
        &env::var("FESEN_UDP").unwrap_or_default(),
        &env::var("FESEN_PUBLIC_UDP_PORT").unwrap_or_default(),
        &env::var("FESEN_STUN").unwrap_or_default(),
    )?;
    anyhow::ensure!(
        origin.scheme() == "https" || listen.ip().is_loopback(),
        "明文 HTTP 只允许监听环回地址"
    );
    let tls = tls::Tls::load(
        env::var_os("FESEN_TLS_CERT").map(PathBuf::from),
        env::var_os("FESEN_TLS_KEY").map(PathBuf::from),
        origin.scheme() == "https",
    )
    .await?;
    let db = store::db(&path)?;
    bootstrap_admin(
        &db,
        env::var("FESEN_ADMIN_USER").ok().as_deref(),
        env::var("FESEN_ADMIN_PASSWORD").ok().as_deref(),
        env::var_os("FESEN_ADMIN_PASSWORD_FILE")
            .map(PathBuf::from)
            .as_deref(),
    )?;
    let tcp = TcpListener::bind(listen).await?;
    let udp = match config.listen {
        Some(listen) => Some(UdpSocket::bind(listen).await?),
        None => None,
    };
    let port = match &udp {
        Some(udp) => config.public_port.unwrap_or(udp.local_addr()?.port()),
        None => 0,
    };
    let app = App::new(db, origin, port, config.stun)?;
    println!(
        "fesen-server {} {}，探测 {}",
        if tls.is_some() { "HTTPS" } else { "HTTP" },
        tcp.local_addr()?,
        match (&udp, &app.stun) {
            (Some(udp), _) => format!("UDP {}", udp.local_addr()?),
            (None, Some(_)) => "STUN".into(),
            (None, None) => "关闭".into(),
        }
    );
    let web = router(app.clone());
    tokio::select! {
        r = async move { match tls {
            Some(tls) => tls.serve(tcp, web).await,
            None => axum::serve(tcp, web.into_make_service_with_connect_info::<SocketAddr>()).await,
        } } => r?,
        r = async move { match udp {
            Some(udp) => control::discovery(udp, app).await,
            None => std::future::pending().await,
        } } => r?,
        () = shutdown() => {},
    }
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    if let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn router(app: App) -> Router {
    let https = app.origin.scheme() == "https";
    let admin_entry = get(|| async { axum::response::Redirect::temporary("/admin/") });
    Router::new()
        .route("/", admin_entry.clone())
        .route("/admin", admin_entry)
        .route("/healthz", get(|| async { "ok" }))
        .route("/join", get(control::join))
        .route("/control", get(control::upgrade))
        .nest("/admin/", admin::router(https))
        .with_state(app)
}
