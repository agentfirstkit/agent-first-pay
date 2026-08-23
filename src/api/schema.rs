//! OpenAPI 3.2 document and standalone JSON Schemas, generated from the DTOs
//! in `api::model`.
//!
//! Nothing here is hand-written structure: every component and every
//! standalone schema is `schemars` output for a Rust type the server actually
//! deserializes into. The path table names those components; the tests below
//! check that every reference resolves, that every mutation declares how it
//! is made idempotent, and that the committed `openapi/` tree still matches
//! this source.

use std::collections::BTreeMap;

use schemars::{JsonSchema, Schema, SchemaGenerator, generate::SchemaSettings};
use serde_json::{Map, Value, json};

use super::model::{
    ApiErrorEnvelope, BalanceListQuery, BalanceListResult, CashuRedemptionRequest,
    CashuRedemptionResult, CashuTokenPlanRequest, CashuTokenResult, HealthResult,
    PayConfirmRequest, PayPlanResult, ReceiveClaimRequest, ReceiveClaimedResult,
    ReceiveCreateRequest, ReceiveResult, SendPlanRequest, SendResult, SpendLimitListResult,
    TransactionListQuery, TransactionListResult, TransactionStatusResult, TransactionSyncRequest,
    TransactionSyncResult, WalletClosedResult, WalletCreateRequest, WalletCreatedResult,
    WalletDetailResult, WalletListQuery, WalletListResult,
};

pub(crate) const JSON_SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";
const SCHEMA_BASE: &str = "https://agentfirstkit.com/schemas/afpay/v1";

struct RegisteredSchema {
    slug: &'static str,
    component: &'static str,
    schema: Value,
}

pub(crate) fn standalone_schemas() -> BTreeMap<String, Value> {
    registered_schemas()
        .into_iter()
        .map(|entry| {
            let filename = format!("{}.schema.json", entry.slug);
            let mut schema = entry.schema;
            if let Some(object) = schema.as_object_mut() {
                object.insert(
                    "$id".to_string(),
                    json!(format!("{SCHEMA_BASE}/{filename}")),
                );
                object.insert("title".to_string(), json!(entry.component));
            }
            (filename, schema)
        })
        .collect()
}

pub(crate) fn schema_index() -> Value {
    let schemas = registered_schemas()
        .into_iter()
        .map(|entry| {
            json!({
                "schema_name": entry.slug,
                "schema_url": format!("/schemas/{}.schema.json", entry.slug),
                "component_name": entry.component,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema_name": "afpay_schema_index",
        "schema_version": 1,
        "json_schema_dialect": JSON_SCHEMA_DIALECT,
        "count": schemas.len(),
        "schemas": schemas,
    })
}

pub(crate) fn openapi_document() -> Value {
    let mut component_schemas = Map::new();
    for entry in registered_schemas() {
        let mut schema = entry.schema;
        if let Some(object) = schema.as_object_mut() {
            object.remove("$schema");
            object.remove("$id");
        }
        component_schemas.insert(entry.component.to_string(), schema);
    }

    json!({
        "openapi": "3.2.0",
        "$self": "/openapi.json",
        "jsonSchemaDialect": JSON_SCHEMA_DIALECT,
        "info": {
            "title": "Agent-First Pay API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "The HTTP domain API of an afpay daemon: wallets, balances, receives, sends, transactions, and the spend limits every payment is checked against. Sends move money and require an Idempotency-Key, which afpay persists for 24 hours and replays rather than re-broadcasting. Operations that read or write key material, spend-limit rules, or daemon config are deliberately absent from this transport: they exist only on the local CLI.",
        },
        "servers": [{"url": "/", "description": "The afpay daemon that served this document"}],
        "tags": [
            {"name": "discovery"},
            {"name": "wallets"},
            {"name": "balances"},
            {"name": "receives"},
            {"name": "sends"},
            {"name": "transactions"},
            {"name": "spend-limits"}
        ],
        "security": [{"bearerAuth": []}],
        "paths": paths(),
        "components": {
            "securitySchemes": {
                "bearerAuth": {
                    "type": "http",
                    "scheme": "bearer",
                    "description": "Pass the API key in Authorization. Credentials in the query string are rejected."
                }
            },
            "parameters": {
                "IdempotencyKey": {
                    "name": "Idempotency-Key",
                    "in": "header",
                    "required": true,
                    "description": "Stable caller-generated key for this exact request. afpay persists it with a canonical hash of the body for 24 hours: the same key with the same body replays the first terminal outcome instead of doing the work twice, and the same key with a different body is refused with `idempotency_conflict`. Required on every operation a retry could duplicate — a payment, a wallet whose key material would be generated again, a receive whose invoice a payer may already hold.",
                    "schema": {
                        "type": "string",
                        "minLength": 8,
                        "maxLength": 128,
                        "pattern": "^[A-Za-z0-9][A-Za-z0-9._-]{7,127}$"
                    }
                }
            },
            "schemas": component_schemas,
        }
    })
}

fn registered_schemas() -> Vec<RegisteredSchema> {
    vec![
        registered::<ApiErrorEnvelope>("api-error-envelope", "ApiErrorEnvelope"),
        registered::<HealthResult>("health-result", "HealthResult"),
        registered::<WalletListQuery>("wallet-list-query", "WalletListQuery"),
        registered::<WalletListResult>("wallet-list-result", "WalletListResult"),
        registered::<WalletCreateRequest>("wallet-create-request", "WalletCreateRequest"),
        registered::<WalletCreatedResult>("wallet-created-result", "WalletCreatedResult"),
        registered::<WalletDetailResult>("wallet-detail-result", "WalletDetailResult"),
        registered::<WalletClosedResult>("wallet-closed-result", "WalletClosedResult"),
        registered::<BalanceListQuery>("balance-list-query", "BalanceListQuery"),
        registered::<BalanceListResult>("balance-list-result", "BalanceListResult"),
        registered::<ReceiveCreateRequest>("receive-create-request", "ReceiveCreateRequest"),
        registered::<ReceiveResult>("receive-result", "ReceiveResult"),
        registered::<ReceiveClaimRequest>("receive-claim-request", "ReceiveClaimRequest"),
        registered::<ReceiveClaimedResult>("receive-claimed-result", "ReceiveClaimedResult"),
        registered::<SendPlanRequest>("send-plan-request", "SendPlanRequest"),
        registered::<PayPlanResult>("pay-plan-result", "PayPlanResult"),
        registered::<PayConfirmRequest>("pay-confirm-request", "PayConfirmRequest"),
        registered::<SendResult>("send-result", "SendResult"),
        registered::<CashuTokenPlanRequest>("cashu-token-plan-request", "CashuTokenPlanRequest"),
        registered::<CashuTokenResult>("cashu-token-result", "CashuTokenResult"),
        registered::<CashuRedemptionRequest>("cashu-redemption-request", "CashuRedemptionRequest"),
        registered::<CashuRedemptionResult>("cashu-redemption-result", "CashuRedemptionResult"),
        registered::<TransactionListQuery>("transaction-list-query", "TransactionListQuery"),
        registered::<TransactionListResult>("transaction-list-result", "TransactionListResult"),
        registered::<TransactionStatusResult>(
            "transaction-status-result",
            "TransactionStatusResult",
        ),
        registered::<TransactionSyncRequest>("transaction-sync-request", "TransactionSyncRequest"),
        registered::<TransactionSyncResult>("transaction-sync-result", "TransactionSyncResult"),
        registered::<SpendLimitListResult>("spend-limit-list-result", "SpendLimitListResult"),
    ]
}

fn registered<T: JsonSchema>(slug: &'static str, component: &'static str) -> RegisteredSchema {
    RegisteredSchema {
        slug,
        component,
        schema: schema_for::<T>(),
    }
}

fn schema_for<T: JsonSchema>() -> Value {
    let settings = SchemaSettings::draft2020_12().with(|settings| {
        settings.inline_subschemas = true;
    });
    let generator = SchemaGenerator::new(settings);
    let schema: Schema = generator.into_root_schema_for::<T>();
    Value::from(schema)
}

fn paths() -> Value {
    json!({
        "/health": {
            "get": public_operation("get_health", "Check daemon health and version", "HealthResult")
        },
        "/openapi.json": {
            "get": raw_public_operation("get_openapi_document", "Read the served OpenAPI contract", "application/vnd.oai.openapi+json;version=3.2")
        },
        "/schemas/index.json": {
            "get": raw_public_operation("list_json_schemas", "List standalone JSON Schemas", "application/json")
        },
        "/schemas/{schema_file}": {
            "get": raw_public_operation_with_parameters(
                "get_json_schema",
                "Read one standalone JSON Schema",
                "application/schema+json",
                vec![path_parameter("schema_file", "Schema filename ending in .schema.json", string_schema())],
            )
        },
        "/v1/wallets": {
            "get": operation(
                "list_wallets",
                "List wallets this daemon holds",
                "wallets",
                query_parameters::<WalletListQuery>(),
                None,
                "WalletListResult",
                Behavior::Read,
            ),
            "post": operation(
                "create_wallet",
                "Create a wallet on one network. Not naturally idempotent — without a supplied mnemonic the daemon generates one, so a repeat derives a different id and leaves the caller holding two wallets it cannot tell apart. The Idempotency-Key is therefore required, and a replay reports the wallet the first call created. A replay never re-emits a generated mnemonic: read it on the machine that holds the wallet.",
                "wallets",
                vec![],
                Some("WalletCreateRequest"),
                "WalletCreatedResult",
                Behavior::KeyedMutation,
            )
        },
        "/v1/wallets/{wallet}": {
            "get": operation(
                "get_wallet",
                "Read one wallet's stored configuration",
                "wallets",
                vec![wallet_path_parameter()],
                None,
                "WalletDetailResult",
                Behavior::Read,
            ),
            "delete": operation(
                "close_wallet",
                "Close a wallet. Refuses while the balance is non-zero. Naturally idempotent: a second call reports the wallet is gone.",
                "wallets",
                vec![wallet_path_parameter()],
                None,
                "WalletClosedResult",
                Behavior::ConvergentMutation,
            )
        },
        "/v1/balances": {
            "get": operation(
                "list_balances",
                "Read balances across wallets",
                "balances",
                query_parameters::<BalanceListQuery>(),
                None,
                "BalanceListResult",
                Behavior::Read,
            )
        },
        "/v1/receives": {
            "post": operation(
                "create_receive",
                "Create a receiving address, invoice, or mint quote. Not naturally idempotent for Lightning or Cashu — a repeat mints a second invoice while a payer may already be holding the first, and the caller then watches the wrong one. The Idempotency-Key is therefore required, and a replay returns the receive already handed out. A request that also waits for settlement holds its key Pending until it settles, so a retry gets `idempotency_in_progress` rather than a second invoice.",
                "receives",
                vec![],
                Some("ReceiveCreateRequest"),
                "ReceiveResult",
                Behavior::KeyedMutation,
            )
        },
        "/v1/receives/{quote_id}/claim": {
            "post": operation(
                "claim_receive",
                "Claim a paid mint quote into the wallet. Naturally idempotent: the mint issues proofs for a quote once.",
                "receives",
                vec![path_parameter("quote_id", "Quote id returned by create_receive", string_schema())],
                Some("ReceiveClaimRequest"),
                "ReceiveClaimedResult",
                Behavior::ConvergentMutation,
            )
        },
        "/v1/send-plans": {
            "post": operation(
                "plan_send",
                "Resolve a payment into a reviewable plan: the wallet afpay would use, what leaves it, what the network charges, and the spend budgets it debits. Nothing is broadcast and no value moves — POST /v1/sends with the returned plan_id is what pays. Repeating this call resolves another plan against current network conditions; nothing has moved, so there is no outcome to replay.",
                "sends",
                vec![],
                Some("SendPlanRequest"),
                "PayPlanResult",
                Behavior::ConvergentMutation,
            )
        },
        "/v1/sends": {
            "post": operation(
                "create_send",
                "Pay by confirming a plan that was reviewed. The body carries only the plan_id: what executes is read from the stored plan, so an approved payment and the payment made cannot differ. The plan is refused if it expired, if it was already confirmed, or if the workspace, daemon configuration, wallet or spend rules changed since it was resolved. Checked against every spend limit before it is broadcast; the reservation is confirmed only after the network accepts it.",
                "sends",
                vec![],
                Some("PayConfirmRequest"),
                "SendResult",
                Behavior::Payment,
            )
        },
        "/v1/cashu/token-plans": {
            "post": operation(
                "plan_cashu_token",
                "Resolve a Cashu bearer-token mint into a reviewable plan. Nothing is minted and no proofs move — POST /v1/cashu/tokens with the returned plan_id is what mints. Repeating this call resolves another plan; nothing has moved, so there is no outcome to replay.",
                "sends",
                vec![],
                Some("CashuTokenPlanRequest"),
                "PayPlanResult",
                Behavior::ConvergentMutation,
            )
        },
        "/v1/cashu/tokens": {
            "post": operation(
                "mint_cashu_token",
                "Mint the bearer token a reviewed plan describes. The response carries the token; whoever holds that string holds the funds. A plan resolved for a send is refused here, and the same staleness, expiry and single-use rules as POST /v1/sends apply.",
                "sends",
                vec![],
                Some("PayConfirmRequest"),
                "CashuTokenResult",
                Behavior::Payment,
            )
        },
        "/v1/cashu/redemptions": {
            "post": operation(
                "redeem_cashu_token",
                "Redeem a Cashu bearer token into a wallet. Naturally idempotent: the mint accepts each proof once, so a replayed token is refused at the source.",
                "receives",
                vec![],
                Some("CashuRedemptionRequest"),
                "CashuRedemptionResult",
                Behavior::ConvergentMutation,
            )
        },
        "/v1/transactions": {
            "get": operation(
                "list_transactions",
                "List recorded payments",
                "transactions",
                query_parameters::<TransactionListQuery>(),
                None,
                "TransactionListResult",
                Behavior::Read,
            )
        },
        "/v1/transactions/{transaction_id}": {
            "get": operation(
                "get_transaction",
                "Read one payment's current settlement status",
                "transactions",
                vec![path_parameter("transaction_id", "Network transaction id", string_schema())],
                None,
                "TransactionStatusResult",
                Behavior::Read,
            )
        },
        "/v1/transactions/sync": {
            "post": operation(
                "sync_transactions",
                "Re-read recent activity from the configured providers into local history. Naturally idempotent: records reconcile by stable network transaction id, so repeating a sync updates the same rows instead of duplicating them.",
                "transactions",
                vec![],
                Some("TransactionSyncRequest"),
                "TransactionSyncResult",
                Behavior::ConvergentMutation,
            )
        },
        "/v1/spend-limits": {
            "get": operation(
                "list_spend_limits",
                "Read every spend-limit rule this daemon enforces and the spend already consumed in each window. The rules themselves are edited only on the machine that holds the ledger.",
                "spend-limits",
                vec![],
                None,
                "SpendLimitListResult",
                Behavior::Read,
            )
        }
    })
}

#[derive(Clone, Copy)]
enum Behavior {
    Read,
    /// A mutation that converges on its own: repeating it lands on the same
    /// state rather than a second one. Its description must say so, which is
    /// what §8's last paragraph asks of an operation that carries no key.
    ConvergentMutation,
    /// A mutation a retry could duplicate, carried by afpay's persistent
    /// idempotency store, which replays the first terminal outcome for a
    /// repeated key. No value leaves a wallet.
    KeyedMutation,
    /// The same, and money leaves a wallet. Confirming a reviewed plan is the
    /// only shape this takes.
    Payment,
}

impl Behavior {
    fn declares_key(self) -> bool {
        matches!(self, Self::KeyedMutation | Self::Payment)
    }
}

fn operation(
    operation_id: &str,
    summary: &str,
    tag: &str,
    mut parameters: Vec<Value>,
    request_schema: Option<&str>,
    result_schema: &str,
    behavior: Behavior,
) -> Value {
    if behavior.declares_key() {
        parameters.push(json!({"$ref": "#/components/parameters/IdempotencyKey"}));
    }
    let mut value = json!({
        "operationId": operation_id,
        "summary": summary,
        "tags": [tag],
        "parameters": parameters,
        "x-afpay-moves-money": matches!(behavior, Behavior::Payment),
        "responses": standard_responses(result_schema),
    });
    if let Some(request_schema) = request_schema {
        value["requestBody"] = json!({
            "required": true,
            "content": {
                "application/json": {
                    "schema": {"$ref": format!("#/components/schemas/{request_schema}")}
                }
            }
        });
    }
    value
}

fn public_operation(operation_id: &str, summary: &str, result_schema: &str) -> Value {
    let mut value = operation(
        operation_id,
        summary,
        "discovery",
        Vec::new(),
        None,
        result_schema,
        Behavior::Read,
    );
    value["security"] = json!([]);
    value
}

fn raw_public_operation(operation_id: &str, summary: &str, media_type: &str) -> Value {
    raw_public_operation_with_parameters(operation_id, summary, media_type, Vec::new())
}

fn raw_public_operation_with_parameters(
    operation_id: &str,
    summary: &str,
    media_type: &str,
    parameters: Vec<Value>,
) -> Value {
    json!({
        "operationId": operation_id,
        "summary": summary,
        "tags": ["discovery"],
        "security": [],
        "parameters": parameters,
        "responses": {
            "200": {
                "description": "Document",
                "content": {(media_type): {"schema": {"type": "object"}}}
            },
            "404": error_response("Not found"),
            "405": error_response("Method not allowed")
        }
    })
}

fn standard_responses(result_schema: &str) -> Value {
    json!({
        "200": {
            "description": "Successful AFDATA result envelope",
            "content": {
                "application/json": {
                    "schema": result_envelope_schema(result_schema)
                }
            }
        },
        "400": error_response("Invalid request"),
        "401": authentication_error_response(),
        "403": error_response("Operator policy refused the request"),
        "404": error_response("Resource not found"),
        "405": error_response("Method not allowed"),
        "409": error_response("Idempotency conflict, or a key whose first request is still in flight"),
        "413": error_response("Request body exceeds the documented limit"),
        "415": error_response("Request body is not application/json"),
        "422": error_response("Request was valid but cannot be applied; a spend limit refusal lands here"),
        "429": error_response("Rate limit exceeded"),
        "500": error_response("Internal contract, storage, or ledger failure"),
        "503": error_response("A provider or upstream node was unreachable")
    })
}

fn result_envelope_schema(result_schema: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["kind", "result", "trace"],
        "properties": {
            "kind": {"const": "result"},
            "result": {"$ref": format!("#/components/schemas/{result_schema}")},
            "trace": {
                "type": "object",
                "additionalProperties": false,
                "required": ["duration_ms"],
                "properties": {"duration_ms": {"type": "integer", "minimum": 0}}
            }
        }
    })
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {
                "schema": {"$ref": "#/components/schemas/ApiErrorEnvelope"}
            }
        }
    })
}

fn authentication_error_response() -> Value {
    let mut response = error_response("Missing or invalid bearer credential");
    response["headers"] = json!({
        "WWW-Authenticate": {
            "description": "Bearer challenge",
            "schema": {"type": "string"}
        }
    });
    response
}

fn wallet_path_parameter() -> Value {
    path_parameter("wallet", "Wallet id or label", string_schema())
}

fn path_parameter(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "in": "path",
        "required": true,
        "description": description,
        "schema": schema,
    })
}

fn query_parameters<T: JsonSchema>() -> Vec<Value> {
    let schema = schema_for::<T>();
    let required = schema["required"].as_array().cloned().unwrap_or_default();
    schema["properties"]
        .as_object()
        .into_iter()
        .flat_map(|properties| properties.iter())
        .map(|(name, property)| {
            let mut property = property.clone();
            normalize_query_parameter_schema(&mut property);
            let mut parameter = json!({
                "name": name,
                "in": "query",
                "required": required.iter().any(|value| value == name),
                "schema": property,
            });
            if let Some(description) = parameter["schema"].get("description").cloned() {
                parameter["description"] = description;
            }
            parameter
        })
        .collect()
}

fn normalize_query_parameter_schema(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if object.get("default").is_some_and(Value::is_null) {
        object.remove("default");
    }
    let Some(types) = object.get("type").and_then(Value::as_array) else {
        return;
    };
    let non_null = types
        .iter()
        .filter(|value| value.as_str() != Some("null"))
        .cloned()
        .collect::<Vec<_>>();
    if non_null.len() == 1 {
        object.insert("type".to_string(), non_null[0].clone());
    } else if !non_null.is_empty() {
        object.insert("type".to_string(), Value::Array(non_null));
    }
}

fn string_schema() -> Value {
    json!({"type": "string", "minLength": 1})
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::{JSON_SCHEMA_DIALECT, openapi_document, schema_index, standalone_schemas};
    use serde_json::Value;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn document_is_openapi_32_with_a_relative_server_and_unique_operation_ids() {
        let document = openapi_document();
        assert_eq!(document["openapi"], "3.2.0");
        assert_eq!(document["jsonSchemaDialect"], JSON_SCHEMA_DIALECT);
        assert_eq!(document["servers"][0]["url"], "/");
        let components = document["components"]["schemas"]
            .as_object()
            .map(|schemas| schemas.keys().cloned().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let mut operation_ids = BTreeSet::new();
        visit(&document["paths"], &mut |value| {
            if let Some(operation_id) = value.get("operationId").and_then(Value::as_str) {
                assert!(
                    operation_ids.insert(operation_id.to_string()),
                    "duplicate operationId {operation_id}"
                );
            }
            if let Some(reference) = value.get("$ref").and_then(Value::as_str)
                && let Some(component) = reference.strip_prefix("#/components/schemas/")
            {
                assert!(components.contains(component), "missing schema {component}");
            }
        });
        assert!(operation_ids.len() >= 18, "{}", operation_ids.len());
    }

    /// The discovery face is the only unauthenticated surface. Anything under
    /// `/v1` inherits the document-level bearer requirement.
    #[test]
    fn only_the_discovery_face_is_public() {
        let document = openapi_document();
        assert_eq!(document["security"][0]["bearerAuth"], json_empty_array());
        for (path, item) in document["paths"].as_object().into_iter().flatten() {
            for (_, operation) in item.as_object().into_iter().flatten() {
                let public = operation["security"] == json_empty_array_value();
                assert_eq!(
                    public,
                    !path.starts_with("/v1"),
                    "{path} public={public} disagrees with its prefix"
                );
            }
        }
    }

    /// Every mutation either takes an `Idempotency-Key` or says in its own
    /// description why it does not need one. Nothing is left to inference, and
    /// nothing declares a key afpay would ignore.
    ///
    /// The keyed set is exactly the operations `Input` carries an
    /// `idempotency_key` field for — payments, wallet creation, and receives —
    /// so a route that grew a key the dispatcher cannot honour would fail
    /// here rather than mislead a caller into trusting a retry.
    #[test]
    fn every_mutation_states_how_a_repeat_behaves() {
        const KEYED: &[&str] = &[
            "create_send",
            "mint_cashu_token",
            "create_wallet",
            "create_receive",
        ];
        let document = openapi_document();
        let mut seen_keyed = BTreeSet::new();
        for (path, item) in document["paths"].as_object().into_iter().flatten() {
            for (method, operation) in item.as_object().into_iter().flatten() {
                if !matches!(method.as_str(), "post" | "put" | "patch" | "delete") {
                    continue;
                }
                let operation_id = operation["operationId"].as_str().unwrap_or_default();
                let declares_key = operation["parameters"]
                    .as_array()
                    .is_some_and(|parameters| {
                        parameters.iter().any(|parameter| {
                            parameter["$ref"] == "#/components/parameters/IdempotencyKey"
                        })
                    });
                assert_eq!(
                    declares_key,
                    KEYED.contains(&operation_id),
                    "{method} {path} disagrees with the set of operations afpay can replay"
                );
                if operation["x-afpay-moves-money"] == true {
                    assert!(declares_key, "{method} {path} moves money without a key");
                }
                if declares_key {
                    seen_keyed.insert(operation_id.to_string());
                    continue;
                }
                let summary = operation["summary"].as_str().unwrap_or_default();
                assert!(
                    summary.contains("idempotent") || summary.contains("Repeating this call"),
                    "{method} {path} never says what a repeat does"
                );
            }
        }
        assert_eq!(
            seen_keyed,
            KEYED
                .iter()
                .map(|id| id.to_string())
                .collect::<BTreeSet<_>>(),
            "an operation that can be replayed lost its Idempotency-Key"
        );
    }

    /// Exactly two operations move value out of a wallet, and both are the
    /// confirm half of a plan. A third would mean a remote effect grew a route
    /// that skips §9's boundary.
    #[test]
    fn only_confirming_a_plan_moves_money() {
        let document = openapi_document();
        let mut money = BTreeSet::new();
        visit(&document["paths"], &mut |value| {
            if value["x-afpay-moves-money"] == true
                && let Some(operation_id) = value["operationId"].as_str()
            {
                money.insert(operation_id.to_string());
            }
        });
        assert_eq!(
            money,
            ["create_send", "mint_cashu_token"]
                .iter()
                .map(|id| id.to_string())
                .collect::<BTreeSet<_>>()
        );
        for (path, plan) in [
            ("/v1/sends", "/v1/send-plans"),
            ("/v1/cashu/tokens", "/v1/cashu/token-plans"),
        ] {
            let body = document["paths"][path]["post"]["requestBody"]["content"]
                ["application/json"]["schema"]["$ref"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                body, "#/components/schemas/PayConfirmRequest",
                "{path} must accept a reviewed plan id and nothing else"
            );
            assert!(
                document["paths"][plan]["post"]["operationId"].is_string(),
                "{path} has no plan operation to confirm"
            );
        }
    }

    #[test]
    fn every_success_names_its_own_result_schema_and_every_failure_the_error_envelope() {
        let document = openapi_document();
        for (path, item) in document["paths"].as_object().into_iter().flatten() {
            for (_, operation) in item.as_object().into_iter().flatten() {
                let responses = &operation["responses"];
                let result = &responses["200"]["content"]["application/json"]["schema"]["properties"]
                    ["result"];
                if !result.is_null() {
                    let reference = result["$ref"]
                        .as_str()
                        .unwrap_or_else(|| panic!("{path} has no typed result ref"));
                    assert!(reference.starts_with("#/components/schemas/"));
                    assert!(!reference.ends_with("ApiErrorEnvelope"));
                }
                for status in ["404", "405"] {
                    assert_eq!(
                        responses[status]["content"]["application/json"]["schema"]["$ref"],
                        "#/components/schemas/ApiErrorEnvelope",
                        "{path} {status}"
                    );
                }
            }
        }
        assert!(
            document["paths"]["/v1/sends"]["post"]["responses"]["401"]["headers"]
                ["WWW-Authenticate"]
                .is_object()
        );
    }

    #[test]
    fn standalone_schemas_use_2020_12_and_have_stable_ids() {
        let schemas = standalone_schemas();
        assert!(schemas.len() >= 25, "{}", schemas.len());
        for (filename, schema) in &schemas {
            assert_eq!(schema["$schema"], JSON_SCHEMA_DIALECT, "{filename}");
            assert!(
                schema["$id"]
                    .as_str()
                    .is_some_and(|id| id.ends_with(filename)),
                "{filename}"
            );
            assert!(
                !contains_remote_reference(schema),
                "{filename} refers to a schema this package does not ship"
            );
        }
        let index = schema_index();
        let indexed = index["schemas"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["schema_name"].as_str())
            .map(|slug| format!("{slug}.schema.json"))
            .collect::<BTreeSet<_>>();
        assert_eq!(indexed, schemas.keys().cloned().collect::<BTreeSet<_>>());
    }

    /// Every request body rejects fields it does not define. A DTO that loses
    /// `deny_unknown_fields` silently starts ignoring an agent's typo.
    #[test]
    fn every_request_body_rejects_unknown_fields() {
        let document = openapi_document();
        for (path, item) in document["paths"].as_object().into_iter().flatten() {
            for (method, operation) in item.as_object().into_iter().flatten() {
                let Some(reference) =
                    operation["requestBody"]["content"]["application/json"]["schema"]["$ref"]
                        .as_str()
                else {
                    continue;
                };
                let component = reference
                    .strip_prefix("#/components/schemas/")
                    .unwrap_or_default();
                let schema = &document["components"]["schemas"][component];
                let closed = schema["additionalProperties"] == false
                    || schema["oneOf"].as_array().is_some_and(|variants| {
                        variants
                            .iter()
                            .all(|variant| variant["additionalProperties"] == false)
                    });
                assert!(closed, "{method} {path} body {component} is open");
            }
        }
    }

    #[test]
    fn committed_contract_matches_the_rust_source() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi");
        let committed: Value = serde_json::from_slice(
            &std::fs::read(root.join("openapi.json")).expect("read committed OpenAPI"),
        )
        .expect("parse committed OpenAPI");
        assert_eq!(
            committed,
            openapi_document(),
            "openapi/openapi.json is stale; run scripts/projects.sh docs agent-first-pay"
        );

        let committed_index: Value = serde_json::from_slice(
            &std::fs::read(root.join("schemas").join("index.json")).expect("read committed index"),
        )
        .expect("parse committed index");
        assert_eq!(committed_index, schema_index());

        let expected = standalone_schemas();
        let committed_names = std::fs::read_dir(root.join("schemas"))
            .expect("read committed schemas")
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".schema.json"))
            .collect::<BTreeSet<_>>();
        assert_eq!(committed_names, expected.keys().cloned().collect());
        for (filename, value) in expected {
            let committed: Value = serde_json::from_slice(
                &std::fs::read(root.join("schemas").join(&filename)).expect("read schema"),
            )
            .expect("parse schema");
            assert_eq!(committed, value, "{filename}");
        }
    }

    fn json_empty_array() -> Value {
        serde_json::json!([])
    }

    fn json_empty_array_value() -> Value {
        serde_json::json!([])
    }

    fn contains_remote_reference(value: &Value) -> bool {
        let mut remote = false;
        visit(value, &mut |node| {
            if let Some(reference) = node.get("$ref").and_then(Value::as_str)
                && !reference.starts_with('#')
            {
                remote = true;
            }
        });
        remote
    }

    fn visit(value: &Value, callback: &mut impl FnMut(&Value)) {
        callback(value);
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, callback);
                }
            }
            Value::Object(values) => {
                for value in values.values() {
                    visit(value, callback);
                }
            }
            _ => {}
        }
    }
}
