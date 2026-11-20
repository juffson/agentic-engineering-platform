use super::state::{Operation, Reply, Store};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::json;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

pub use aep::{CONTROL_DEVICE_TOKEN as DEVICE_TOKEN, CONTROL_OPERATOR_TOKEN as OPERATOR_TOKEN};

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn token(name: &str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("configure {name}"))?;
    ensure!(
        (32..=128).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "{name} must contain 32–128 ASCII letters, digits or hyphens"
    );
    Ok(value)
}

struct Service {
    store: Mutex<Store>,
    operator: String,
    device_token: String,
    device: String,
    ttl: u64,
}

impl Service {
    fn authorized(&self, headers: &HeaderMap, operation: &Operation) -> bool {
        let Some(value) = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
        else {
            return false;
        };
        let expected = if operation.for_device() {
            &self.device_token
        } else {
            &self.operator
        };
        bool::from(value.as_bytes().ct_eq(expected.as_bytes()))
    }
}

async fn command(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    Json(operation): Json<Operation>,
) -> Response {
    if !service.authorized(&headers, &operation) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"authorization required"})),
        )
            .into_response();
    }
    let result = tokio::task::spawn_blocking(move || {
        service
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("control store is unavailable"))?
            .apply(operation, &service.device, now(), service.ttl)
    })
    .await;
    match result {
        Ok(Ok(reply)) => Json(reply).into_response(),
        Ok(Err(error)) => (
            StatusCode::CONFLICT,
            Json(json!({"error":error.to_string()})),
        )
            .into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub fn serve(listen: SocketAddr, db: PathBuf, device: String, ttl: u64) -> Result<()> {
    ensure!(
        listen.ip().is_loopback(),
        "bind to loopback and put a TLS reverse proxy in front for remote access"
    );
    aep::workspace::ident(&device, 63)?;
    ensure!(
        (10..=3600).contains(&ttl),
        "lease must be between 10 and 3600 seconds"
    );
    let operator = token(OPERATOR_TOKEN)?;
    let device_token = token(DEVICE_TOKEN)?;
    ensure!(
        operator != device_token,
        "use distinct device and operator tokens"
    );
    let app = Router::new()
        .route("/v1/control", post(command))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(Arc::new(Service {
            store: Mutex::new(Store::open(db)?),
            operator,
            device_token,
            device,
            ttl,
        }));
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let listener = tokio::net::TcpListener::bind(listen).await?;
            println!("AEP control plane listening on {}", listener.local_addr()?);
            axum::serve(listener, app).await?;
            Ok(())
        })
}

#[derive(Debug)]
pub struct Rejected(pub String);

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

pub struct Client {
    client: reqwest::blocking::Client,
    endpoint: reqwest::Url,
    token: String,
}

impl Client {
    pub fn new(origin: &str, device: bool, ca_cert: Option<&Path>) -> Result<Self> {
        let mut endpoint = reqwest::Url::parse(origin)?;
        ensure!(
            (endpoint.scheme() == "https"
                || (endpoint.scheme() == "http"
                    && matches!(
                        endpoint.host_str(),
                        Some("127.0.0.1" | "[::1]" | "localhost")
                    )))
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.path() == "/"
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "use an HTTPS control origin, or HTTP on loopback"
        );
        ensure!(
            ca_cert.is_none() || endpoint.scheme() == "https",
            "a control CA bundle requires an HTTPS endpoint"
        );
        endpoint.set_path("/v1/control");
        let mut client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5));
        if endpoint.scheme() == "http" {
            client = client.no_proxy();
        }
        if let Some(path) = ca_cert {
            let pem = std::fs::read(path)
                .with_context(|| format!("read CA bundle {}", path.display()))?;
            let roots = reqwest::Certificate::from_pem_bundle(&pem)?;
            ensure!(
                !roots.is_empty(),
                "the control CA bundle contains no certificates"
            );
            client = client.tls_built_in_root_certs(false);
            for root in roots {
                client = client.add_root_certificate(root);
            }
        }
        Ok(Self {
            client: client.build()?,
            endpoint,
            token: token(if device { DEVICE_TOKEN } else { OPERATOR_TOKEN })?,
        })
    }

    pub fn call(&self, operation: Operation) -> Result<Reply> {
        let response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.token)
            .json(&operation)
            .send()?;
        let status = response.status();
        if status == reqwest::StatusCode::CONFLICT {
            let data: serde_json::Value = response.json()?;
            return Err(Rejected(
                data["error"]
                    .as_str()
                    .unwrap_or("control operation rejected")
                    .into(),
            )
            .into());
        }
        Ok(response.error_for_status()?.json()?)
    }
}
