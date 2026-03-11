pub mod crypto;

use crate::handler::{self, App};
use crate::rpc::crypto::Cipher;
use crate::types::*;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::mpsc;
use tonic::Code;
use tonic::{Request, Response, Status};

pub struct RpcInit {
    pub listen: String,
    pub rpc_secret: Option<String>,
    pub log: Vec<String>,
    pub data_dir: Option<String>,
    pub startup_argv: Vec<String>,
    pub startup_args: serde_json::Value,
    pub startup_requested: bool,
}

pub mod proto {
    tonic::include_proto!("afpay");
}

use proto::af_pay_server::{AfPay, AfPayServer};
use proto::{EncryptedRequest, EncryptedResponse};

struct AfPayService {
    cipher: Cipher,
    config: RuntimeConfig,
}

#[tonic::async_trait]
impl AfPay for AfPayService {
    async fn call(
        &self,
        request: Request<EncryptedRequest>,
    ) -> Result<Response<EncryptedResponse>, Status> {
        let req = request.into_inner();

        // Decrypt request
        let plaintext = match self.cipher.decrypt(&req.nonce, &req.ciphertext) {
            Ok(plaintext) => plaintext,
            Err(_) => {
                emit_rpc_request_log(
                    &self.config,
                    None,
                    serde_json::json!({
                        "input": serde_json::Value::Null,
                        "decode_error": "decryption failed",
                    }),
                );
                let status = Status::unauthenticated("decryption failed");
                emit_rpc_response_log(&self.config, None, &[], Some(&status));
                return Err(status);
            }
        };

        let mut raw_input_value = serde_json::from_slice::<serde_json::Value>(&plaintext)
            .unwrap_or(serde_json::Value::Null);
        if let Some(object) = raw_input_value.as_object_mut() {
            object.remove("id");
        }

        // Parse Input
        let input: Input = match serde_json::from_slice(&plaintext) {
            Ok(input) => input,
            Err(e) => {
                emit_rpc_request_log(
                    &self.config,
                    None,
                    serde_json::json!({
                        "input": raw_input_value,
                        "decode_error": format!("invalid input: {e}"),
                    }),
                );
                let status = Status::invalid_argument(format!("invalid input: {e}"));
                emit_rpc_response_log(&self.config, None, &[], Some(&status));
                return Err(status);
            }
        };
        let request_id = input_request_id(&input).map(|s| s.to_string());
        emit_rpc_request_log(
            &self.config,
            request_id.clone(),
            serde_json::json!({
                "input": raw_input_value,
            }),
        );

        // Block local-only operations over RPC
        if input.is_local_only() {
            let status = Status::permission_denied("local-only operation");
            emit_rpc_response_log(&self.config, request_id, &[], Some(&status));
            return Err(status);
        }

        // Create per-request channel and App
        let (tx, mut rx) = mpsc::channel::<Output>(256);
        let store = crate::store::create_storage_backend(&self.config);
        let app = Arc::new(App::new(self.config.clone(), tx, Some(true), store));
        app.requests_total.fetch_add(1, Ordering::Relaxed);

        // Dispatch
        handler::dispatch(&app, input).await;

        // Drop app to close the sender side, then collect all outputs
        drop(app);
        let mut outputs = Vec::new();
        while let Some(out) = rx.recv().await {
            // Mirror server-side log events to rpc daemon stdout so operators can
            // observe request flow in long-running rpc mode.
            if let Output::Log { .. } = &out {
                let rendered = agent_first_data::cli_output(
                    &serde_json::to_value(&out).unwrap_or(serde_json::Value::Null),
                    agent_first_data::OutputFormat::Json,
                );
                println!("{rendered}");
            }
            let value = serde_json::to_value(&out).unwrap_or(serde_json::Value::Null);
            outputs.push(value);
        }

        // Serialize outputs as JSON array
        let response_json = match serde_json::to_vec(&outputs) {
            Ok(response_json) => response_json,
            Err(e) => {
                let status = Status::internal(format!("serialize: {e}"));
                emit_rpc_response_log(&self.config, request_id, &outputs, Some(&status));
                return Err(status);
            }
        };

        // Encrypt response
        let (nonce, ciphertext) = match self.cipher.encrypt(&response_json) {
            Ok(payload) => payload,
            Err(e) => {
                let status = Status::internal(format!("encrypt: {e}"));
                emit_rpc_response_log(&self.config, request_id, &outputs, Some(&status));
                return Err(status);
            }
        };

        emit_rpc_response_log(&self.config, request_id, &outputs, None);

        Ok(Response::new(EncryptedResponse { nonce, ciphertext }))
    }
}

pub async fn run_rpc(init: RpcInit) {
    let secret: String = match init.rpc_secret {
        Some(s) if !s.is_empty() => s,
        _ => {
            let value = agent_first_data::build_cli_error(
                "--rpc-secret is required for RPC mode",
                Some("pass a shared secret for client authentication"),
            );
            let rendered =
                agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
            println!("{rendered}");
            std::process::exit(1);
        }
    };

    let cipher = Cipher::from_secret(&secret);

    let resolved_dir = init
        .data_dir
        .unwrap_or_else(|| RuntimeConfig::default().data_dir);
    let mut config = match RuntimeConfig::load_from_dir(&resolved_dir) {
        Ok(c) => c,
        Err(e) => {
            let value = agent_first_data::build_cli_error(&e, None);
            let rendered =
                agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
            println!("{rendered}");
            std::process::exit(1);
        }
    };
    if !init.log.is_empty() {
        config.log = init.log;
    }

    // Emit startup log
    if let Some(startup) = crate::config::maybe_startup_log(
        &config.log,
        init.startup_requested,
        Some(init.startup_argv),
        Some(&config),
        init.startup_args,
    ) {
        let value = serde_json::to_value(&startup).unwrap_or(serde_json::Value::Null);
        let rendered = agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
        println!("{rendered}");
    }

    let startup_errors = crate::handler::startup_provider_validation_errors(&config).await;
    for error_output in &startup_errors {
        let value = serde_json::to_value(error_output).unwrap_or(serde_json::Value::Null);
        let rendered = agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
        println!("{rendered}");
    }
    if !startup_errors.is_empty() {
        std::process::exit(1);
    }

    let service = AfPayService { cipher, config };

    let addr = match init.listen.parse() {
        Ok(a) => a,
        Err(e) => {
            let value = agent_first_data::build_cli_error(
                &format!("invalid --rpc-listen address: {e}"),
                Some("expected format: host:port (e.g. 127.0.0.1:9100)"),
            );
            let rendered =
                agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
            println!("{rendered}");
            std::process::exit(1);
        }
    };

    let server = tonic::transport::Server::builder()
        .add_service(AfPayServer::new(service))
        .serve(addr);

    if let Err(e) = server.await {
        let value = agent_first_data::build_cli_error(&format!("RPC server error: {e}"), None);
        let rendered = agent_first_data::cli_output(&value, agent_first_data::OutputFormat::Json);
        println!("{rendered}");
        std::process::exit(1);
    }
}

fn log_event_enabled(log_filters: &[String], event: &str) -> bool {
    if log_filters.is_empty() {
        return false;
    }
    let ev = event.to_ascii_lowercase();
    log_filters
        .iter()
        .any(|f| f == "*" || f == "all" || ev.starts_with(f.as_str()))
}

fn emit_rpc_request_log(
    config: &RuntimeConfig,
    request_id: Option<String>,
    args: serde_json::Value,
) {
    emit_rpc_log(config, "rpc_request", request_id, args);
}

fn emit_rpc_response_log(
    config: &RuntimeConfig,
    request_id: Option<String>,
    outputs: &[serde_json::Value],
    status: Option<&Status>,
) {
    let has_output_error = outputs
        .iter()
        .any(|item| item.get("code").and_then(|v| v.as_str()) == Some("error"));
    let mut args = serde_json::json!({
        "output_count": outputs.len(),
        "has_error": has_output_error || status.is_some(),
        "outputs": outputs,
    });
    if let Some(status) = status {
        if let Some(object) = args.as_object_mut() {
            object.insert(
                "grpc_error".to_string(),
                serde_json::json!({
                    "code": grpc_code_name(status.code()),
                    "message": status.message(),
                }),
            );
        }
    }
    emit_rpc_log(config, "rpc_response", request_id, args);
}

fn emit_rpc_log(
    config: &RuntimeConfig,
    event: &str,
    request_id: Option<String>,
    args: serde_json::Value,
) {
    if !log_event_enabled(&config.log, event) {
        return;
    }
    let log = Output::Log {
        event: event.to_string(),
        request_id,
        version: None,
        argv: None,
        config: None,
        args: Some(args),
        env: None,
        trace: Trace::from_duration(0),
    };
    let rendered = agent_first_data::cli_output(
        &serde_json::to_value(&log).unwrap_or(serde_json::Value::Null),
        agent_first_data::OutputFormat::Json,
    );
    println!("{rendered}");
}

fn grpc_code_name(code: Code) -> &'static str {
    match code {
        Code::Ok => "ok",
        Code::Cancelled => "cancelled",
        Code::Unknown => "unknown",
        Code::InvalidArgument => "invalid_argument",
        Code::DeadlineExceeded => "deadline_exceeded",
        Code::NotFound => "not_found",
        Code::AlreadyExists => "already_exists",
        Code::PermissionDenied => "permission_denied",
        Code::ResourceExhausted => "resource_exhausted",
        Code::FailedPrecondition => "failed_precondition",
        Code::Aborted => "aborted",
        Code::OutOfRange => "out_of_range",
        Code::Unimplemented => "unimplemented",
        Code::Internal => "internal",
        Code::Unavailable => "unavailable",
        Code::DataLoss => "data_loss",
        Code::Unauthenticated => "unauthenticated",
    }
}

fn input_request_id(input: &Input) -> Option<&str> {
    match input {
        Input::WalletCreate { id, .. }
        | Input::LnWalletCreate { id, .. }
        | Input::WalletClose { id, .. }
        | Input::WalletList { id, .. }
        | Input::Balance { id, .. }
        | Input::Receive { id, .. }
        | Input::ReceiveClaim { id, .. }
        | Input::CashuSend { id, .. }
        | Input::CashuReceive { id, .. }
        | Input::Send { id, .. }
        | Input::Restore { id, .. }
        | Input::WalletShowSeed { id, .. }
        | Input::HistoryList { id, .. }
        | Input::HistoryStatus { id, .. }
        | Input::HistoryUpdate { id, .. }
        | Input::LimitAdd { id, .. }
        | Input::LimitRemove { id, .. }
        | Input::LimitList { id, .. }
        | Input::LimitSet { id, .. }
        | Input::WalletConfigShow { id, .. }
        | Input::WalletConfigSet { id, .. }
        | Input::WalletConfigTokenAdd { id, .. }
        | Input::WalletConfigTokenRemove { id, .. } => Some(id.as_str()),
        Input::Config(_) | Input::Version | Input::Close => None,
    }
}
