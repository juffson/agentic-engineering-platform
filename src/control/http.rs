use super::{
    evidence,
    state::{Operation, Reply, Store},
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::{Bytes, to_bytes},
    extract::{DefaultBodyLimit, Path as RoutePath, Request, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{ErrorKind, Read},
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
        valid_token(&value),
        "{name} must contain 32–128 ASCII letters, digits or hyphens"
    );
    Ok(value)
}

fn valid_token(value: &str) -> bool {
    (32..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

struct Service {
    store: Mutex<Store>,
    operator: String,
    workers: BTreeMap<String, String>,
    device: String,
    ttl: u64,
}

impl Service {
    fn is_operator(&self, headers: &HeaderMap) -> bool {
        bearer(headers)
            .is_some_and(|value| bool::from(value.as_bytes().ct_eq(self.operator.as_bytes())))
    }

    fn worker(&self, headers: &HeaderMap) -> Option<&str> {
        let token = bearer(headers)?;
        self.workers.iter().find_map(|(worker, expected)| {
            bool::from(token.as_bytes().ct_eq(expected.as_bytes())).then_some(worker.as_str())
        })
    }
}

async fn command(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    Json(operation): Json<Operation>,
) -> Response {
    let identity = if operation.for_device() {
        service.worker(&headers)
    } else {
        service
            .is_operator(&headers)
            .then_some(service.device.as_str())
    };
    let Some(identity) = identity.map(str::to_owned) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"authorization required"})),
        )
            .into_response();
    };
    let result = tokio::task::spawn_blocking(move || {
        service
            .store
            .lock()
            .map_err(|_| anyhow::anyhow!("control store is unavailable"))?
            .apply(operation, &identity, now(), service.ttl)
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

fn scope(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn evidence_path(
    service: &Service,
    id: &str,
    ticket: Option<&str>,
    proof: Option<(&str, &str)>,
) -> Result<PathBuf> {
    service
        .store
        .lock()
        .map_err(|_| anyhow::anyhow!("control store is unavailable"))?
        .evidence_path(id, ticket, proof)
}

async fn upload(
    State(service): State<Arc<Service>>,
    RoutePath(run): RoutePath<String>,
    request: Request,
) -> Response {
    let Some(worker) = service.worker(request.headers()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(fence) = scope(request.headers(), "x-aep-fence") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(path) = evidence_path(&service, &run, None, Some((worker, &fence))) else {
        return StatusCode::CONFLICT.into_response();
    };
    let Ok(bytes) = to_bytes(request.into_body(), evidence::LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    if evidence::validate(&bytes).is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match tokio::task::spawn_blocking(move || -> Result<bool> {
        if path.exists() {
            return Ok(fs::read(path)? == bytes);
        }
        fs::create_dir_all(path.parent().context("evidence directory")?)?;
        if let Err(error) = evidence::persist(&path, &bytes) {
            if path.is_file() {
                return Ok(fs::read(path)? == bytes);
            }
            return Err(error);
        }
        Ok(true)
    })
    .await
    {
        Ok(Ok(true)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Ok(false)) => StatusCode::CONFLICT.into_response(),
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn download(
    State(service): State<Arc<Service>>,
    RoutePath(run): RoutePath<String>,
    headers: HeaderMap,
) -> Response {
    if !service.is_operator(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(ticket) = scope(&headers, "x-aep-ticket") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(path) = evidence_path(&service, &run, Some(&ticket), None) else {
        return StatusCode::CONFLICT.into_response();
    };
    match tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(evidence::LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        Ok::<_, std::io::Error>(bytes)
    })
    .await
    {
        Ok(Ok(bytes)) if bytes.len() <= evidence::LIMIT => Bytes::from(bytes).into_response(),
        Ok(Err(error)) if error.kind() == ErrorKind::NotFound => {
            StatusCode::NOT_FOUND.into_response()
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub fn serve(
    listen: SocketAddr,
    db: PathBuf,
    device: String,
    ttl: u64,
    extra_workers: Option<&Path>,
) -> Result<()> {
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
    let mut workers = BTreeMap::from([(device.clone(), device_token)]);
    if let Some(path) = extra_workers {
        let bytes = fs::read(path).context("read Worker credentials")?;
        ensure!(
            bytes.len() <= 16 * 1024,
            "Worker configuration is too large"
        );
        let extra: BTreeMap<String, String> =
            serde_json::from_slice(&bytes).context("parse Worker credentials")?;
        ensure!(extra.len() <= 32, "too many configured Workers");
        for (id, token) in extra {
            aep::workspace::ident(&id, 63)?;
            ensure!(
                !workers.contains_key(&id)
                    && valid_token(&token)
                    && token != operator
                    && !workers.values().any(|existing| *existing == token),
                "invalid or duplicate Worker credentials for {id}"
            );
            workers.insert(id, token);
        }
    }
    let app = Router::new()
        .route(
            "/v1/control",
            post(command).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/v1/evidence/{run}", get(download).put(upload))
        .with_state(Arc::new(Service {
            store: Mutex::new(Store::open(db)?),
            operator,
            workers,
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

    fn evidence_url(&self, run: &str) -> Result<reqwest::Url> {
        aep::workspace::ident(run, 64)?;
        let mut url = self.endpoint.clone();
        url.set_path(&format!("/v1/evidence/{run}"));
        Ok(url)
    }

    pub fn upload(&self, run: &str, fence: &str, packet: Vec<u8>) -> Result<()> {
        let response = self
            .client
            .put(self.evidence_url(run)?)
            .timeout(Duration::from_secs(40))
            .bearer_auth(&self.token)
            .header("x-aep-fence", fence)
            .body(packet)
            .send()?;
        if response.status().is_client_error() {
            return Err(
                Rejected(format!("evidence upload rejected: {}", response.status())).into(),
            );
        }
        response.error_for_status()?;
        Ok(())
    }

    pub fn download(&self, ticket: &str, run: &str) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.client
            .get(self.evidence_url(run)?)
            .timeout(Duration::from_secs(40))
            .bearer_auth(&self.token)
            .header("x-aep-ticket", ticket)
            .send()?
            .error_for_status()?
            .take(evidence::LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        evidence::validate(&bytes)?;
        Ok(bytes)
    }
}
