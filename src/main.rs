use std::{net::SocketAddr, env};

use axum::{
    body::{Body, BoxBody},
    debug_handler,
    http::{self, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    Router, routing::get, extract::Path, Json,
};
use hyper::upgrade::Upgraded;
use sqlx::PgPool;
use anyhow::Result;
use tokio::net::TcpStream;
use tower::{make::Shared, ServiceExt};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "roads_proxy=trace".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let pool = PgPool::connect(&env::var("DATABASE_URL")?).await?;
    let _ = add_route(&pool, "/");

    let router_svc = Router::new()
        .route("/ping", get(|| async { "Pong" }))
        .route("/", get(redirect));

    let env_port = std::env::var("PORT");
    let port: u16 = env_port.unwrap_or_else(|_| "3000".into()).parse().unwrap();

    let service = tower::service_fn(move |req: Request<Body>| {
        let router_svc = router_svc.clone();
        let req = req.map(Body::from);
        async move {
            if req.method() == Method::CONNECT {
                proxy(req).await
            } else {
                router_svc.oneshot(req).await.map_err(|err| match err {})
            }
        }
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tracing::debug!("listening on {}", addr);

    axum::Server::bind(&addr)
        .serve(Shared::new(service))
        .await
        .expect("server error");

    Ok(())
}

async fn get_route(conn: &PgPool, route: &str) -> Result<()> {
    let _ = sqlx::query!(r#"
SELECT redirect_to
FROM routes
WHERE route = $1
"#, route)
    .fetch_all(conn)
    .await?;

    Ok(())
}

async fn add_route(conn: &PgPool, route: &str) -> Result<()> {
    let _ = sqlx::query!(r#"
INSERT INTO routes ( route, redirect_to )
VALUES ( $1, $2 )
"#, route, "teste_teste")
    .fetch_one(conn)
    .await?;

    Ok(())
}

#[debug_handler]
async fn redirect() -> impl IntoResponse {
    (Response::builder()
         .status(StatusCode::PERMANENT_REDIRECT)
         .header("Location", "https://bento.me/devjunio")
         .body(())
         .unwrap()
     , ("Redirecting to website..."));
}

/// Proxies a CONNECT request to the destination address.
async fn proxy(req: Request<Body>) -> http::Result<Response> {
    tracing::trace!(?req);

    if let Some(host_addr) = req.uri().authority().map(std::string::ToString::to_string) {
        tokio::task::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => {
                    if let Err(e) = tunnel(upgraded, host_addr).await {
                        tracing::warn!("server io error: {e}");
                    };
                }
                Err(e) => tracing::warn!("upgrade error: {e}"),
            }
        });

        Ok(Response::new(BoxBody::default()))
    } else {
        tracing::warn!("CONNECT host is not socket addr: {:?}", req.uri());
        Ok((
            StatusCode::BAD_REQUEST,
            "CONNECT must be to a socket address",
        )
            .into_response())
    }
}

/// Tunnels the client stream to the server stream.
async fn tunnel(mut upgraded: Upgraded, addr: String) -> std::io::Result<()> {
    let mut server = TcpStream::connect(addr).await?;

    // It returns a future that swaps copies from `upgraded` with `server`
    let (from_client, from_server) =
        tokio::io::copy_bidirectional(&mut upgraded, &mut server).await?;

    tracing::debug!(
        "client wrote {} bytes and received {} bytes",
        from_client,
        from_server
        );

    Ok(())
}
