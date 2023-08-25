use axum::{
    body::Body,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
    body::BoxBody,
    http::Request
};
use std::net::SocketAddr;
use hyper::upgrade::Upgraded;
use tokio::net::TcpStream;
use tower::{make::Shared, ServiceExt};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
    .with(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "example_http_proxy=trace,tower_http=debug".into()),
    )
    .with(tracing_subscriber::fmt::layer())
    .init();

    let env_port = std::env::var("PORT");
    let port: u16 = env_port
        .unwrap_or_else(|_| "3000".into())
        .parse()
        .expect("PORT must be a number");

    let router_svc = Router::new()
        .route(
            "/ping",
            get(|| async { "Pong" })
        );

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
}

/// Proxies a CONNECT request to the destination address.
async fn proxy(req: Request<Body>) -> Result<Response, hyper::Error> {
    tracing::trace!(?req);

    if let Some(host_addr) = req.uri().authority().map(std::string::ToString::to_string) {
        tokio::task::spawn(async move {
            match hyper::upgrade::on(req).await {
                Ok(upgraded) => {
                    if let Err(e) = tunnel(upgraded, host_addr).await {
                        tracing::warn!("server io error: {}", e);
                    };
                }
                Err(e) => tracing::warn!("upgrade error: {}", e),
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

    // `tokio::io::copy_bidirectional` copies from `upgraded` to `server` and from
    // `server` to `upgraded` in parallel.
    //
    // It returns a future that resolves to the number of bytes copied from
    // `upgraded` to `server` and the number of bytes copied from `server` to
    // `upgraded`.
    let (from_client, from_server) =
        tokio::io::copy_bidirectional(&mut upgraded, &mut server).await?;

    tracing::debug!(
        "client wrote {} bytes and received {} bytes",
        from_client,
        from_server
    );

    Ok(())
}