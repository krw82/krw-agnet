//! Opt-in transport acceptance against a real, pinned MCP endpoint.
//!
//! This is deliberately skipped in normal CI. Operators enable it only with
//! an explicit endpoint, exact readiness pins, and a PEM trust anchor. The
//! test keeps data payloads out of stdout while proving the Rust TLS client,
//! readiness admission, MCP initialize, tools/list, and one root `SearchPlan`
//! call against a real immutable ontology release.

use std::env;
use std::fs;
use std::time::Duration;

use krw_agent_protocol::{ContentHash, McpToolSessionReuse};
use krw_agent_tool_mcp::{ExpectedFingerprint, McpHttpClient, McpHttpConfig};
use serde_json::{Value, json};
use zeroize::Zeroizing;

const ENABLE_ENV: &str = "KRW_LIVE_MCP_SMOKE";

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} is required when {ENABLE_ENV}=1"))
}

#[tokio::test]
async fn rust_tls_client_reaches_pinned_real_ontology_mcp() {
    if env::var(ENABLE_ENV).as_deref() != Ok("1") {
        return;
    }

    let certificate_path = required("KRW_LIVE_MCP_CA_PEM_FILE");
    let certificate = fs::read_to_string(&certificate_path)
        .unwrap_or_else(|_| panic!("cannot read {certificate_path}"));
    let _ = rustls::crypto::ring::default_provider().install_default();
    // Keep the first failure legible for an operator: this is the same pinned
    // PEM trust anchor that the production MCP client receives below, without
    // printing the endpoint, certificate, or any response payload.
    let direct_roots = reqwest::Certificate::from_pem_bundle(certificate.as_bytes())
        .expect("valid PEM trust bundle");
    let mut direct_builder = reqwest::Client::builder().no_proxy();
    for root in direct_roots {
        direct_builder = direct_builder.add_root_certificate(root);
    }
    let direct_client = direct_builder.build().expect("build direct TLS client");
    let direct_health = direct_client
        .get(required("KRW_LIVE_MCP_READY_URL"))
        .send()
        .await
        .expect("certificate-verified TLS readiness request");
    assert!(direct_health.status().is_success());
    let config = McpHttpConfig {
        endpoint: required("KRW_LIVE_MCP_URL"),
        readiness_endpoint: required("KRW_LIVE_MCP_READY_URL"),
        origin: required("KRW_LIVE_MCP_ORIGIN"),
        bearer_token: None,
        protocol_version: "2025-06-18".into(),
        client_name: "krw-agent-live-tls-smoke".into(),
        client_version: env!("CARGO_PKG_VERSION").into(),
        tls_profile: "system-plus-pinned-ca-v1".into(),
        tls_ca_pem: Some(Zeroizing::new(certificate)),
        max_concurrency: 1,
        request_timeout: Duration::from_mins(1),
        max_response_bytes: 256 * 1024,
    };
    let expected = ExpectedFingerprint {
        server_build: required("KRW_LIVE_MCP_SERVER_BUILD"),
        server_schema_bundle_hash: ContentHash::parse(required("KRW_LIVE_MCP_TOOL_SCHEMA_SHA256"))
            .expect("tool schema hash"),
        data_release_hash: ContentHash::parse(required("KRW_LIVE_MCP_RELEASE_MANIFEST_SHA256"))
            .expect("release manifest hash"),
        protocol_version: "2025-06-18".into(),
        tool_session_reuse: McpToolSessionReuse::RunScoped,
    };

    let client = McpHttpClient::connect(config, expected)
        .await
        .expect("pinned TLS MCP initialization");
    let tools = client.list_tools().await.expect("tools/list");
    let tool_list = tools
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools/list array");
    assert_eq!(tool_list.len(), 27);
    assert!(tool_list.iter().any(|tool| {
        tool.get("name").and_then(Value::as_str) == Some("krw_ontology_query_context")
    }));

    let outcome = client
        .call_tool(
            "krw_ontology_query_context",
            json!({
                "question": "AAPL 매출 추이를 공시 기준으로 보여줘.",
                "intent": "metric_trend",
                "tickers": ["AAPL"],
                "document_types": ["10-K"],
                "clauses": [{
                    "clause_id": "revenue_trend",
                    "retrieval_query": "AAPL revenue trend",
                    "metrics": ["revenue"],
                    "metric_scope": "company_total"
                }],
                "limit_results": 8
            }),
        )
        .await
        .expect("actual root SearchPlan call");
    let (payload, tool_error) = outcome.into_payload();
    assert!(
        !tool_error,
        "root SearchPlan must not return a tool-level correction"
    );
    let state = payload
        .get("structuredContent")
        .and_then(Value::as_object)
        .expect("structured ResearchState");
    let answerability = state
        .get("answerability")
        .and_then(Value::as_object)
        .expect("answerability");
    assert_eq!(
        answerability.get("status").and_then(Value::as_str),
        Some("answerable")
    );
    assert_eq!(
        answerability
            .get("strong_claim_allowed")
            .and_then(Value::as_bool),
        Some(true)
    );
}
