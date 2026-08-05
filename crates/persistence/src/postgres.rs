//! Bounded `PostgreSQL` transport for the fixed `agent_v1` JSON procedure ABI.
//!
//! This module deliberately has no arbitrary-query entry point. Every pooled
//! connection prepares the complete procedure inventory before it is admitted
//! to the pool. Connection strings and optional private CA material are read
//! once through an environment-name indirection and are never included in a
//! `Debug` or tracing value.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use krw_agent_protocol::ContentHash;
use rustls::{ClientConfig, RootCertStore};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::timeout;
use tokio_postgres::config::{SslMode, TargetSessionAttrs};
use tokio_postgres::types::Json;
use tokio_postgres::{Client, Statement};
use tokio_postgres_rustls::MakeRustlsConnect;
use tracing::{debug, warn};
use zeroize::Zeroizing;

use crate::agent_v1::ABI_VERSION;
use crate::agent_v1::{AgentV1Procedure, DatabaseFailure, JsonProcedureExecutor};

const PROCEDURES: [AgentV1Procedure; 21] = [
    AgentV1Procedure::EnqueueRun,
    AgentV1Procedure::ClaimRun,
    AgentV1Procedure::RenewLease,
    AgentV1Procedure::CheckpointEpisode,
    AgentV1Procedure::CheckpointRunState,
    AgentV1Procedure::BeginAction,
    AgentV1Procedure::ObserveAction,
    AgentV1Procedure::FinalizeAction,
    AgentV1Procedure::MarkActionAmbiguous,
    AgentV1Procedure::CheckpointChildExecution,
    AgentV1Procedure::ReadChildExecution,
    AgentV1Procedure::ReadSessionMemory,
    AgentV1Procedure::CheckpointSessionMemorySnapshot,
    AgentV1Procedure::CommitFinal,
    AgentV1Procedure::RequestCancel,
    AgentV1Procedure::FailOrDefer,
    AgentV1Procedure::ClaimOutbox,
    AgentV1Procedure::AckOutbox,
    AgentV1Procedure::ReadCommittedOutcome,
    AgentV1Procedure::ReadFinalOutput,
    AgentV1Procedure::ReapRetainedRuns,
];

const MIN_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_TIMEOUT: Duration = Duration::from_mins(2);
const MAX_POOL_CONNECTIONS: usize = 64;
const MAX_MIN_IDLE: usize = 8;

/// Non-secret deployment settings. `url_env` and `ca_pem_env` name secret
/// sources; they never contain the values themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostgresPoolOptions {
    pub url_env: String,
    pub ca_pem_env: Option<String>,
    pub application_name: String,
    pub max_connections: usize,
    pub min_idle: usize,
    pub connect_timeout: Duration,
    pub checkout_timeout: Duration,
    pub query_timeout: Duration,
    pub statement_timeout: Duration,
    pub lock_timeout: Duration,
}

impl Default for PostgresPoolOptions {
    fn default() -> Self {
        Self {
            url_env: "KRW_AGENT_DATABASE_URL".into(),
            ca_pem_env: None,
            application_name: "krw-agentd".into(),
            max_connections: 8,
            min_idle: 1,
            connect_timeout: Duration::from_secs(5),
            checkout_timeout: Duration::from_secs(2),
            query_timeout: Duration::from_secs(15),
            statement_timeout: Duration::from_secs(14),
            lock_timeout: Duration::from_secs(2),
        }
    }
}

pub trait DatabaseSecretSource {
    fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, PostgresSetupError>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnvironmentDatabaseSecrets;

impl DatabaseSecretSource for ProcessEnvironmentDatabaseSecrets {
    fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, PostgresSetupError> {
        validate_env_name(name)?;
        let value = std::env::var(name)
            .map_err(|_| PostgresSetupError::MissingSecretRef(name.to_owned()))?;
        if value.is_empty() {
            return Err(PostgresSetupError::MissingSecretRef(name.to_owned()));
        }
        Ok(Zeroizing::new(value))
    }
}

#[derive(Debug, Error)]
pub enum PostgresSetupError {
    #[error("invalid database environment reference")]
    InvalidSecretRef,
    #[error("database secret reference is unavailable: {0}")]
    MissingSecretRef(String),
    #[error("invalid bounded PostgreSQL pool setting: {0}")]
    InvalidPoolSetting(&'static str),
    #[error("invalid PostgreSQL connection configuration")]
    InvalidConnectionConfiguration,
    #[error("could not load a trusted root certificate")]
    InvalidRootCertificate,
    #[error("PostgreSQL startup connection timed out")]
    ConnectTimeout,
    #[error("PostgreSQL startup readiness failed ({0})")]
    ReadinessFailed(ContentHash),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostgresPoolStats {
    pub max_connections: usize,
    pub idle_connections: usize,
    pub checked_out_connections: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PostgresAbiReadiness {
    pub abi_version: u16,
    pub prepared_procedures: usize,
    pub ready_connections: usize,
}

/// Concrete, cloneable executor for [`crate::agent_v1::AgentV1Client`].
#[derive(Clone)]
pub struct PostgresJsonExecutor {
    inner: Arc<PoolInner>,
}

impl fmt::Debug for PostgresJsonExecutor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresJsonExecutor")
            .field("max_connections", &self.inner.max_connections)
            .field("query_timeout", &self.inner.query_timeout)
            .field("endpoint", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

struct PoolInner {
    postgres: tokio_postgres::Config,
    tls: ClientConfig,
    idle: Mutex<Vec<PooledConnection>>,
    permits: Arc<Semaphore>,
    max_connections: usize,
    connect_timeout: Duration,
    checkout_timeout: Duration,
    query_timeout: Duration,
}

impl fmt::Debug for PoolInner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PoolInner")
            .field("postgres", &"[REDACTED]")
            .field("max_connections", &self.max_connections)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct PooledConnection {
    client: Client,
    statements: BTreeMap<AgentV1Procedure, Statement>,
}

struct Checkout {
    connection: Option<PooledConnection>,
    idle: Arc<PoolInner>,
    _permit: OwnedSemaphorePermit,
    reusable: bool,
}

impl fmt::Debug for Checkout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Checkout")
            .field("has_connection", &self.connection.is_some())
            .field("reusable", &self.reusable)
            .finish_non_exhaustive()
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let Some(connection) = self.connection.take() else {
            return;
        };
        if !self.reusable || connection.client.is_closed() {
            return;
        }
        let Ok(mut idle) = self.idle.idle.lock() else {
            return;
        };
        if idle.len() < self.idle.max_connections {
            idle.push(connection);
        }
    }
}

impl PostgresJsonExecutor {
    /// Resolve secrets, require certificate-verified TLS, establish the
    /// configured minimum idle set, and prepare every ABI statement. Successful
    /// construction is the daemon's database readiness check.
    pub async fn connect(
        options: PostgresPoolOptions,
        secrets: &impl DatabaseSecretSource,
    ) -> Result<Self, PostgresSetupError> {
        validate_options(&options)?;
        // `rustls-no-provider` keeps the crypto backend explicit. Install the
        // approved ring provider before any client configuration is built; a
        // prior MCP client may already have installed the same provider.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let database_url = secrets.read_secret(&options.url_env)?;
        let mut postgres = database_url
            .parse::<tokio_postgres::Config>()
            .map_err(|_| PostgresSetupError::InvalidConnectionConfiguration)?;
        postgres
            .ssl_mode(SslMode::Require)
            .target_session_attrs(TargetSessionAttrs::ReadWrite)
            .connect_timeout(options.connect_timeout)
            .application_name(&options.application_name)
            .options(format!(
                "-c statement_timeout={} -c lock_timeout={} -c idle_in_transaction_session_timeout={}",
                duration_millis(options.statement_timeout),
                duration_millis(options.lock_timeout),
                duration_millis(options.query_timeout)
            ));
        drop(database_url);

        let tls = build_tls_config(options.ca_pem_env.as_deref(), secrets)?;
        let inner = Arc::new(PoolInner {
            postgres,
            tls,
            idle: Mutex::new(Vec::with_capacity(options.max_connections)),
            permits: Arc::new(Semaphore::new(options.max_connections)),
            max_connections: options.max_connections,
            connect_timeout: options.connect_timeout,
            checkout_timeout: options.checkout_timeout,
            query_timeout: options.query_timeout,
        });
        let executor = Self { inner };
        for _ in 0..options.min_idle {
            let connection = executor.open_connection().await?;
            executor
                .inner
                .idle
                .lock()
                .map_err(|_| {
                    PostgresSetupError::ReadinessFailed(ContentHash::sha256("pool_lock_poisoned"))
                })?
                .push(connection);
        }
        debug!(
            min_idle = options.min_idle,
            max_connections = options.max_connections,
            procedures = PROCEDURES.len(),
            "PostgreSQL agent_v1 ABI is ready"
        );
        Ok(executor)
    }

    pub fn stats(&self) -> PostgresPoolStats {
        let idle_connections = self
            .inner
            .idle
            .lock()
            .map_or(0, |connections| connections.len());
        PostgresPoolStats {
            max_connections: self.inner.max_connections,
            idle_connections,
            checked_out_connections: self
                .inner
                .max_connections
                .saturating_sub(self.inner.permits.available_permits()),
        }
    }

    /// Readiness is established without claiming work: every admitted idle
    /// connection has successfully prepared the full fixed procedure inventory.
    /// The v1 schema has no separate migration-version query, so ABI preparation
    /// is the strongest side-effect-free startup check it exposes.
    pub fn readiness(&self) -> PostgresAbiReadiness {
        PostgresAbiReadiness {
            abi_version: ABI_VERSION,
            prepared_procedures: PROCEDURES.len(),
            ready_connections: self
                .inner
                .idle
                .lock()
                .map_or(0, |connections| connections.len()),
        }
    }

    async fn checkout(&self) -> Result<Checkout, DatabaseFailure> {
        let permit = timeout(
            self.inner.checkout_timeout,
            Arc::clone(&self.inner.permits).acquire_owned(),
        )
        .await
        .map_err(|_| DatabaseFailure::redacted(None, "postgres_pool_checkout_timeout"))?
        .map_err(|_| DatabaseFailure::redacted(None, "postgres_pool_closed"))?;
        let pooled = self
            .inner
            .idle
            .lock()
            .map_err(|_| DatabaseFailure::redacted(None, "postgres_pool_lock_poisoned"))?
            .pop();
        let connection = match pooled {
            Some(connection) if !connection.client.is_closed() => connection,
            _ => self
                .open_connection()
                .await
                .map_err(|error| DatabaseFailure::redacted(None, format!("{error:?}")))?,
        };
        Ok(Checkout {
            connection: Some(connection),
            idle: Arc::clone(&self.inner),
            _permit: permit,
            // A cancelled execute future may drop this checkout while the
            // server still processes its request. Admit it back only after a
            // complete protocol response has been observed below.
            reusable: false,
        })
    }

    async fn open_connection(&self) -> Result<PooledConnection, PostgresSetupError> {
        let connector = MakeRustlsConnect::new(self.inner.tls.clone());
        let (client, connection) = timeout(
            self.inner.connect_timeout,
            self.inner.postgres.connect(connector),
        )
        .await
        .map_err(|_| PostgresSetupError::ConnectTimeout)?
        .map_err(|error| redacted_readiness(&error))?;
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                warn!(
                    diagnostic_hash = %ContentHash::sha256(format!("{error:?}")),
                    "PostgreSQL pooled connection closed"
                );
            }
        });

        let mut statements = BTreeMap::new();
        for procedure in PROCEDURES {
            let statement = timeout(
                self.inner.connect_timeout,
                client.prepare(procedure.statement()),
            )
            .await
            .map_err(|_| PostgresSetupError::ConnectTimeout)?
            .map_err(|error| redacted_readiness(&error))?;
            statements.insert(procedure, statement);
        }
        Ok(PooledConnection { client, statements })
    }
}

#[async_trait]
impl JsonProcedureExecutor for PostgresJsonExecutor {
    async fn execute_json(
        &self,
        procedure: AgentV1Procedure,
        request: Value,
    ) -> Result<Value, DatabaseFailure> {
        let mut checkout = self.checkout().await?;
        let connection = checkout
            .connection
            .as_ref()
            .ok_or_else(|| DatabaseFailure::redacted(None, "postgres_checkout_empty"))?;
        let statement = connection
            .statements
            .get(&procedure)
            .ok_or_else(|| DatabaseFailure::redacted(None, "postgres_statement_missing"))?;
        let result = timeout(
            self.inner.query_timeout,
            connection.client.query_one(statement, &[&Json(request)]),
        )
        .await;
        let row = match result {
            Ok(Ok(row)) => {
                checkout.reusable = true;
                row
            }
            Ok(Err(error)) => {
                checkout.reusable = !error.is_closed();
                let sqlstate = error
                    .as_db_error()
                    .map(|database_error| database_error.code().code().to_owned());
                return Err(DatabaseFailure::redacted(sqlstate, format!("{error:?}")));
            }
            Err(_) => {
                // Dropping a query future does not prove that PostgreSQL stopped
                // executing it. Never return this connection to the pool.
                checkout.reusable = false;
                return Err(DatabaseFailure::redacted(
                    None,
                    "postgres_procedure_timeout",
                ));
            }
        };
        row.try_get::<_, Json<Value>>(0)
            .map(|Json(value)| value)
            .map_err(|error| DatabaseFailure::redacted(None, format!("{error:?}")))
    }
}

fn validate_options(options: &PostgresPoolOptions) -> Result<(), PostgresSetupError> {
    validate_env_name(&options.url_env)?;
    if let Some(reference) = &options.ca_pem_env {
        validate_env_name(reference)?;
    }
    if options.application_name.is_empty()
        || options.application_name.len() > 63
        || !options
            .application_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(PostgresSetupError::InvalidPoolSetting("application_name"));
    }
    if !(1..=MAX_POOL_CONNECTIONS).contains(&options.max_connections)
        || !(1..=options.max_connections).contains(&options.min_idle)
        || options.min_idle > MAX_MIN_IDLE
    {
        return Err(PostgresSetupError::InvalidPoolSetting("connection bounds"));
    }
    for (name, value) in [
        ("connect_timeout", options.connect_timeout),
        ("checkout_timeout", options.checkout_timeout),
        ("query_timeout", options.query_timeout),
        ("statement_timeout", options.statement_timeout),
        ("lock_timeout", options.lock_timeout),
    ] {
        if !(MIN_TIMEOUT..=MAX_TIMEOUT).contains(&value) {
            return Err(PostgresSetupError::InvalidPoolSetting(name));
        }
    }
    if options.statement_timeout >= options.query_timeout {
        return Err(PostgresSetupError::InvalidPoolSetting(
            "statement_timeout must be below query_timeout",
        ));
    }
    Ok(())
}

fn build_tls_config(
    ca_pem_env: Option<&str>,
    secrets: &impl DatabaseSecretSource,
) -> Result<ClientConfig, PostgresSetupError> {
    let native = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    for certificate in native.certs {
        roots
            .add(certificate)
            .map_err(|_| PostgresSetupError::InvalidRootCertificate)?;
    }
    if let Some(reference) = ca_pem_env {
        let pem = secrets.read_secret(reference)?;
        let mut cursor = Cursor::new(pem.as_bytes());
        let mut added = 0_usize;
        for certificate in rustls_pemfile::certs(&mut cursor) {
            roots
                .add(certificate.map_err(|_| PostgresSetupError::InvalidRootCertificate)?)
                .map_err(|_| PostgresSetupError::InvalidRootCertificate)?;
            added += 1;
        }
        if added == 0 {
            return Err(PostgresSetupError::InvalidRootCertificate);
        }
    }
    if roots.is_empty() {
        return Err(PostgresSetupError::InvalidRootCertificate);
    }
    Ok(ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth())
}

fn validate_env_name(name: &str) -> Result<(), PostgresSetupError> {
    let mut bytes = name.bytes();
    let valid_first = bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic());
    if !valid_first || !bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric()) {
        return Err(PostgresSetupError::InvalidSecretRef);
    }
    Ok(())
}

fn duration_millis(duration: Duration) -> u128 {
    duration.as_millis()
}

fn redacted_readiness(error: &tokio_postgres::Error) -> PostgresSetupError {
    PostgresSetupError::ReadinessFailed(ContentHash::sha256(format!("{error:?}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct Secrets(BTreeMap<String, String>);

    impl fmt::Debug for Secrets {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("Secrets")
                .field("entries", &self.0.len())
                .finish_non_exhaustive()
        }
    }

    impl DatabaseSecretSource for Secrets {
        fn read_secret(&self, name: &str) -> Result<Zeroizing<String>, PostgresSetupError> {
            self.0
                .get(name)
                .cloned()
                .map(Zeroizing::new)
                .ok_or_else(|| PostgresSetupError::MissingSecretRef(name.into()))
        }
    }

    #[test]
    fn debug_never_contains_database_url() {
        let options = PostgresPoolOptions::default();
        let debug = format!("{options:?}");
        assert!(debug.contains("KRW_AGENT_DATABASE_URL"));
        assert!(!debug.contains("postgres://"));
    }

    #[test]
    fn rejects_unbounded_or_inverted_pool_options() {
        let mut options = PostgresPoolOptions {
            max_connections: 0,
            ..PostgresPoolOptions::default()
        };
        assert!(validate_options(&options).is_err());
        options.max_connections = 8;
        options.statement_timeout = options.query_timeout;
        assert!(validate_options(&options).is_err());
        options.statement_timeout = Duration::from_secs(5);
        options.url_env = "bad-name".into();
        assert!(validate_options(&options).is_err());
    }

    #[test]
    fn secret_source_is_reference_only() {
        let secrets = Secrets(BTreeMap::from([(
            "DB_URL".into(),
            "postgres://user:password@example.invalid/database".into(),
        )]));
        let secret = secrets.read_secret("DB_URL").unwrap();
        assert!(secret.contains("password"));
        assert!(!format!("{secrets:?}").contains("example.invalid"));
    }
}
