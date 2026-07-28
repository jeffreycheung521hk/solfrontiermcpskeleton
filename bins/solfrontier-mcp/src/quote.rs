//! Read-only Jupiter quote transport, policy envelope, and response mapping.
//!
//! The narrow `JupiterQuoteSource` seam and policy order are adapted from
//! predecessor path `crates/gateway/src/tools/get_jupiter_quote.rs` at
//! commit `d202a74c31b8ed3f592789abd6ef3f2f84dda14e`.
//!
//! The quote wire types and `GET /swap/v1/quote` transport are adapted from
//! predecessor path `crates/gateway/src/integrations/jupiter.rs` at
//! commit `d423ca170b4cbbfc8dc3e48ae95cb78ba6e7564d`. That response shape was
//! checked against `api.jup.ag` in April 2026. Execution/build/swap types and
//! endpoints are intentionally omitted: Phase 1 is read-only.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) const SOL_MINT_BS58: &str = "So11111111111111111111111111111111111111112";
pub(crate) const USDC_MINT_BS58: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub(crate) const MAX_SLIPPAGE_BPS: u16 = 100;

const JUPITER_API_BASE_URL: &str = "https://api.jup.ag";
const QUOTE_TIMEOUT: Duration = Duration::from_secs(8);

const STATUS_OK: &str = "ok";
const STATUS_POLICY_BLOCKED: &str = "policy_blocked";
const STATUS_API_ERROR: &str = "api_error";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwapQuoteRequest {
    pub(crate) input_mint: String,
    pub(crate) output_mint: String,
    pub(crate) amount: u64,
    pub(crate) slippage_bps: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SwapMode {
    ExactIn,
    ExactOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwapQuoteResponse {
    pub(crate) input_mint: String,
    pub(crate) in_amount: String,
    pub(crate) output_mint: String,
    pub(crate) out_amount: String,
    pub(crate) other_amount_threshold: String,
    pub(crate) swap_mode: SwapMode,
    pub(crate) slippage_bps: u16,
    #[serde(default)]
    pub(crate) price_impact_pct: Option<String>,
    #[serde(default)]
    pub(crate) route_plan: Vec<RoutePlanStep>,
    #[serde(default)]
    pub(crate) context_slot: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RoutePlanStep {
    pub(crate) swap_info: SwapInfo,
    pub(crate) percent: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SwapInfo {
    pub(crate) amm_key: String,
    #[serde(default)]
    pub(crate) label: Option<String>,
    pub(crate) input_mint: String,
    pub(crate) output_mint: String,
    pub(crate) in_amount: String,
    pub(crate) out_amount: String,
    #[serde(default)]
    pub(crate) fee_amount: Option<String>,
    #[serde(default)]
    pub(crate) fee_mint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum QuoteReadError {
    #[error("Jupiter quote request timed out")]
    Timeout,
    #[error("Jupiter quote request failed")]
    RequestFailed,
    #[error("Jupiter quote API returned HTTP {0}")]
    HttpStatus(u16),
    #[error("Jupiter quote response was invalid")]
    InvalidResponse,
}

/// Narrow read-only seam. Production uses [`HttpJupiterClient`]; tests inject
/// an offline mock. No build, signing, transaction, or broadcast API exists.
#[allow(async_fn_in_trait)]
pub(crate) trait JupiterQuoteSource: Send + Sync {
    async fn fetch_quote(
        &self,
        request: &SwapQuoteRequest,
    ) -> Result<SwapQuoteResponse, QuoteReadError>;
}

/// Quote-only Jupiter client.
///
/// The configured base URL is never logged or returned. Raw reqwest errors and
/// provider response bodies are discarded because either may contain a URL,
/// query string, or untrusted upstream content.
#[derive(Clone)]
pub(crate) struct HttpJupiterClient {
    http: reqwest::Client,
    base_url: String,
}

impl HttpJupiterClient {
    fn with_base_url(base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_owned(),
        }
    }
}

impl JupiterQuoteSource for HttpJupiterClient {
    async fn fetch_quote(
        &self,
        request: &SwapQuoteRequest,
    ) -> Result<SwapQuoteResponse, QuoteReadError> {
        let endpoint = format!("{}/swap/v1/quote", self.base_url);
        let response =
            tokio::time::timeout(QUOTE_TIMEOUT, self.http.get(endpoint).query(request).send())
                .await
                .map_err(|_| QuoteReadError::Timeout)?
                .map_err(|_| QuoteReadError::RequestFailed)?;

        let status = response.status();
        if !status.is_success() {
            let status = status.as_u16();
            tracing::warn!(
                http_status = status,
                "Jupiter quote API returned a non-success status"
            );
            return Err(QuoteReadError::HttpStatus(status));
        }

        tokio::time::timeout(QUOTE_TIMEOUT, response.json::<SwapQuoteResponse>())
            .await
            .map_err(|_| QuoteReadError::Timeout)?
            .map_err(|_| QuoteReadError::InvalidResponse)
    }
}

/// Use a non-empty Unicode override, otherwise the public Jupiter endpoint.
/// Invalid endpoint syntax is deliberately deferred to a normal `api_error`
/// tool result so server startup and the other tools remain available.
pub(crate) fn configured_client_from_env() -> HttpJupiterClient {
    let base_url = std::env::var_os("SOLFRONTIER_JUPITER_BASE_URL")
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| JUPITER_API_BASE_URL.to_owned());
    HttpJupiterClient::with_base_url(base_url)
}

pub(crate) async fn query_quote<S: JupiterQuoteSource>(
    source: &S,
    input_mint: &str,
    output_mint: &str,
    amount: &str,
    slippage_bps: u64,
) -> Value {
    let input_mint = normalize_mint(input_mint);
    let output_mint = normalize_mint(output_mint);

    let amount = match amount.parse::<u64>() {
        Ok(amount) if amount > 0 => amount,
        _ => {
            return policy_blocked(
                "input-amount-invalid",
                "amount must be a positive base-unit integer encoded as a string",
                None,
            );
        }
    };

    if slippage_bps > u64::from(MAX_SLIPPAGE_BPS) {
        return policy_blocked(
            "slippage-exceeds-quote-cap",
            "slippage_bps exceeds the Phase 1 cap of 100",
            Some(slippage_bps),
        );
    }
    let slippage_bps = slippage_bps as u16;

    if !is_allowed_mint(&input_mint) {
        return policy_blocked(
            "input-mint-not-allowed",
            "input_mint is outside the SOL/USDC allowlist",
            None,
        );
    }
    if !is_allowed_mint(&output_mint) {
        return policy_blocked(
            "output-mint-not-allowed",
            "output_mint is outside the SOL/USDC allowlist",
            None,
        );
    }
    if input_mint == output_mint {
        return policy_blocked(
            "input-output-mint-equal",
            "input_mint and output_mint must differ",
            None,
        );
    }

    let request = SwapQuoteRequest {
        input_mint: input_mint.clone(),
        output_mint: output_mint.clone(),
        amount,
        slippage_bps,
    };

    match source.fetch_quote(&request).await {
        Ok(response) => {
            let route_summary: Vec<&str> = response
                .route_plan
                .iter()
                .filter_map(|step| step.swap_info.label.as_deref())
                .collect();
            json!({
                "status": STATUS_OK,
                "input_mint": input_mint,
                "output_mint": output_mint,
                "input_amount": amount.to_string(),
                "in_amount": response.in_amount,
                "out_amount": response.out_amount,
                "other_amount_threshold": response.other_amount_threshold,
                "price_impact_pct": response.price_impact_pct,
                "route_summary": route_summary,
                "slippage_bps": slippage_bps,
                "context_slot": response.context_slot,
            })
        }
        Err(error) => {
            tracing::warn!(
                error_class = quote_error_class(error),
                "Jupiter quote request failed"
            );
            json!({
                "status": STATUS_API_ERROR,
                "reason": error.to_string(),
            })
        }
    }
}

fn is_allowed_mint(mint: &str) -> bool {
    mint == SOL_MINT_BS58 || mint == USDC_MINT_BS58
}

fn normalize_mint(input: &str) -> String {
    let trimmed = input.trim();
    match trimmed.to_ascii_uppercase().as_str() {
        "SOL" | "WSOL" => SOL_MINT_BS58.to_owned(),
        "USDC" => USDC_MINT_BS58.to_owned(),
        _ => trimmed.to_owned(),
    }
}

fn policy_blocked(rule_name: &str, reason: &str, slippage_bps: Option<u64>) -> Value {
    let mut response = json!({
        "status": STATUS_POLICY_BLOCKED,
        "policy_rule_name": rule_name,
        "reason": reason,
    });
    if let Some(slippage_bps) = slippage_bps {
        response["slippage_bps"] = json!(slippage_bps);
    }
    response
}

fn quote_error_class(error: QuoteReadError) -> &'static str {
    match error {
        QuoteReadError::Timeout => "timeout",
        QuoteReadError::RequestFailed => "request_failed",
        QuoteReadError::HttpStatus(_) => "http_status",
        QuoteReadError::InvalidResponse => "invalid_response",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct MockQuoteSource {
        result: Mutex<Result<SwapQuoteResponse, QuoteReadError>>,
        requests: Mutex<Vec<SwapQuoteRequest>>,
    }

    impl MockQuoteSource {
        fn returning(result: Result<SwapQuoteResponse, QuoteReadError>) -> Self {
            Self {
                result: Mutex::new(result),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn request_count(&self) -> usize {
            self.requests.lock().expect("requests lock").len()
        }
    }

    impl JupiterQuoteSource for MockQuoteSource {
        async fn fetch_quote(
            &self,
            request: &SwapQuoteRequest,
        ) -> Result<SwapQuoteResponse, QuoteReadError> {
            self.requests
                .lock()
                .expect("requests lock")
                .push(request.clone());
            self.result.lock().expect("result lock").clone()
        }
    }

    fn quote_fixture() -> SwapQuoteResponse {
        SwapQuoteResponse {
            input_mint: SOL_MINT_BS58.to_owned(),
            in_amount: "18446744073709551615".to_owned(),
            output_mint: USDC_MINT_BS58.to_owned(),
            out_amount: "9007199254740993".to_owned(),
            other_amount_threshold: "8917127262193583".to_owned(),
            swap_mode: SwapMode::ExactIn,
            slippage_bps: 50,
            price_impact_pct: Some("0.00042".to_owned()),
            route_plan: vec![RoutePlanStep {
                swap_info: SwapInfo {
                    amm_key: "safe-amm-pubkey".to_owned(),
                    label: Some("MockDEX".to_owned()),
                    input_mint: SOL_MINT_BS58.to_owned(),
                    output_mint: USDC_MINT_BS58.to_owned(),
                    in_amount: "18446744073709551615".to_owned(),
                    out_amount: "9007199254740993".to_owned(),
                    fee_amount: Some("42".to_owned()),
                    fee_mint: Some(USDC_MINT_BS58.to_owned()),
                },
                percent: 100,
            }],
            context_slot: Some(321_654_987),
        }
    }

    #[tokio::test]
    async fn ok_maps_quote_and_preserves_amount_strings() {
        let source = MockQuoteSource::returning(Ok(quote_fixture()));

        let response = query_quote(&source, " SOL ", "usdc", "18446744073709551615", 50).await;

        assert_eq!(response["status"], STATUS_OK);
        assert_eq!(response["input_mint"], SOL_MINT_BS58);
        assert_eq!(response["output_mint"], USDC_MINT_BS58);
        assert_eq!(response["input_amount"], "18446744073709551615");
        assert_eq!(response["in_amount"], "18446744073709551615");
        assert_eq!(response["out_amount"], "9007199254740993");
        assert_eq!(response["other_amount_threshold"], "8917127262193583");
        assert!(response["input_amount"].is_string());
        assert!(response["in_amount"].is_string());
        assert!(response["out_amount"].is_string());
        assert!(response["other_amount_threshold"].is_string());
        assert_eq!(response["route_summary"], json!(["MockDEX"]));

        let requests = source.requests.lock().expect("requests lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].input_mint, SOL_MINT_BS58);
        assert_eq!(requests[0].output_mint, USDC_MINT_BS58);
        assert_eq!(requests[0].amount, u64::MAX);
        assert_eq!(requests[0].slippage_bps, 50);
    }

    #[tokio::test]
    async fn non_allowlisted_mint_is_policy_blocked_without_provider_call() {
        let source = MockQuoteSource::returning(Ok(quote_fixture()));

        let response = query_quote(
            &source,
            "DezXAZ8z7PnrnRJjz3wXBoRgixCa6j6dx7yAknL2gPj",
            USDC_MINT_BS58,
            "1000",
            50,
        )
        .await;

        assert_eq!(response["status"], STATUS_POLICY_BLOCKED);
        assert_eq!(response["policy_rule_name"], "input-mint-not-allowed");
        assert_eq!(source.request_count(), 0);
    }

    #[tokio::test]
    async fn slippage_above_cap_is_policy_blocked_without_provider_call() {
        let source = MockQuoteSource::returning(Ok(quote_fixture()));

        let response = query_quote(&source, SOL_MINT_BS58, USDC_MINT_BS58, "1000", 101).await;

        assert_eq!(response["status"], STATUS_POLICY_BLOCKED);
        assert_eq!(response["policy_rule_name"], "slippage-exceeds-quote-cap");
        assert_eq!(response["slippage_bps"], 101);
        assert_eq!(source.request_count(), 0);
    }

    #[tokio::test]
    async fn same_mint_is_policy_blocked_without_provider_call() {
        let source = MockQuoteSource::returning(Ok(quote_fixture()));

        let response = query_quote(&source, SOL_MINT_BS58, SOL_MINT_BS58, "1000", 50).await;

        assert_eq!(response["status"], STATUS_POLICY_BLOCKED);
        assert_eq!(response["policy_rule_name"], "input-output-mint-equal");
        assert_eq!(source.request_count(), 0);
    }

    #[tokio::test]
    async fn api_error_is_fixed_json_and_server_facing_payload_is_sanitized() {
        let source = MockQuoteSource::returning(Err(QuoteReadError::RequestFailed));

        let response = query_quote(&source, SOL_MINT_BS58, USDC_MINT_BS58, "1000", 50).await;
        let serialized = response.to_string();

        assert_eq!(response["status"], STATUS_API_ERROR);
        assert_eq!(response["reason"], "Jupiter quote request failed");
        assert_eq!(source.request_count(), 1);
        for forbidden in [
            "http://",
            "https://",
            "api-key",
            "provider-body-sentinel",
            "raw-reqwest-error",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "api_error payload must not contain `{forbidden}`"
            );
        }
    }

    #[test]
    fn module_has_no_write_signing_or_raw_error_path() {
        const SOURCE: &str = include_str!("quote.rs");
        let forbidden = [
            [".", "post("].concat(),
            [".text", "().await"].concat(),
            ["error ", "= %"].concat(),
            ["base_url ", "= %"].concat(),
            ["build_", "swap"].concat(),
            ["send_", "transaction"].concat(),
            ["sign_", "transaction"].concat(),
            ["broadcast_", "transaction"].concat(),
            ["Keypair", "::new("].concat(),
            ["private", "_key"].concat(),
            ["tx", "_bytes"].concat(),
        ];

        for needle in forbidden {
            assert!(
                !SOURCE.contains(&needle),
                "quote module must not contain `{needle}`"
            );
        }
    }
}
