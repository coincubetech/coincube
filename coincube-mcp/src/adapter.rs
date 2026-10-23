//! Official SDK owns MCP lifecycle, negotiation and JSON-RPC serialization.
use crate::*;
use rmcp::{
    ErrorData, ServerHandler,
    model::*,
    service::{RequestContext, RoleServer},
};
use serde_json::Value;
use std::path::PathBuf;

/// Official-SDK handler for public build metadata and four closed desktop reads.
#[derive(Clone)]
pub struct Adapter {
    /// Authenticated local endpoint supplied by trusted host configuration.
    pub socket: PathBuf,
    /// Private desktop-issued bearer credential; never a model tool argument.
    pub credential: String,
}
/// Attach typed schemas and read-only metadata; desktop grants still enforce access.
fn tool<I: JsonSchema + 'static, O: JsonSchema + 'static>(
    name: &'static str,
    description: &'static str,
) -> Tool {
    Tool::new(name, description, serde_json::Map::new())
        .with_input_schema::<I>()
        .with_output_schema::<O>()
        .annotate(
            ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(false),
        )
}
/// Advertise implemented tools without exposing wallet data or secrets.
pub fn tools() -> Vec<Tool> {
    vec![
        tool::<Empty, Capabilities>(
            "tenshu_get_capabilities",
            "Describe implemented build capabilities only. No installed-wallet inventory or live consent. Unsupported creation/proposals are reported honestly.",
        ),
        tool::<Empty, WalletList>(
            "tenshu_list_wallets",
            "List only the Vault explicitly authorized in the current desktop session.",
        ),
        tool::<WalletInput, Summary>(
            "tenshu_get_wallet_summary",
            "Read authorized Vault balance snapshot in satoshis; inspect sync_state for completeness.",
        ),
        tool::<HistoryInput, History>(
            "tenshu_get_transactions",
            "Read at most 50 recent confirmed transactions; excludes labels, addresses and scripts.",
        ),
        tool::<WalletInput, Policy>(
            "tenshu_get_wallet_policy_summary",
            "Deterministic Vault policy analysis without keys or descriptors. Recovery delays are relative blocks, not current eligibility.",
        ),
        tool::<proposal::PrepareInput, proposal::Proposal>(
            "tenshu_prepare_wallet_proposal",
            "Prepare a bounded session-only Regtest 3-of-5 topology draft with explicitly chosen recovery. Requires separate proposal consent; keys and descriptor validation happen in Tenshu. Never creates a wallet.",
        ).annotate(ToolAnnotations::new().read_only(false).destructive(false).idempotent(true).open_world(false)),
        tool::<proposal::ProposalInput, proposal::Proposal>(
            "tenshu_get_wallet_proposal",
            "Read only the current preparation grant's proposal. A topology draft is not a validated descriptor or created wallet.",
        ),
    ]
}
/// Decode typed arguments while withholding raw serialization error details.
fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, ErrorData> {
    // Serde can accept positional arrays for structs, including [] for Empty.
    // MCP arguments must match the advertised object schema at this boundary.
    if !v.is_object() {
        return Err(ErrorData::invalid_params("invalid_input", None));
    }
    serde_json::from_value(v).map_err(|_| ErrorData::invalid_params("invalid_input", None))
}
impl Adapter {
    /// Validate and dispatch a known tool through the desktop authorization gate.
    /// Protocol errors reject unknown/invalid calls; wallet errors are sanitized
    /// structured tool errors rather than raw daemon exceptions.
    pub async fn invoke(&self, name: &str, arguments: Value) -> Result<CallToolResult, ErrorData> {
        // Like tools/list, this is public static metadata. It never touches the
        // desktop, wallet service, session or grant, even when IPC is unavailable.
        if name == "tenshu_get_capabilities" {
            parse::<Empty>(arguments)?;
            let value = serde_json::to_value(Capabilities::current())
                .map_err(|_| ErrorData::internal_error("serialization_failed", None))?;
            return Ok(CallToolResult::structured(value));
        }
        let request = match name {
            "tenshu_list_wallets" => crate::Request::List(parse(arguments)?),
            "tenshu_get_wallet_summary" => crate::Request::Summary(parse(arguments)?),
            "tenshu_get_transactions" => crate::Request::History(parse(arguments)?),
            "tenshu_get_wallet_policy_summary" => crate::Request::Policy(parse(arguments)?),
            "tenshu_prepare_wallet_proposal" => crate::Request::PrepareProposal(parse(arguments)?),
            "tenshu_get_wallet_proposal" => crate::Request::Proposal(parse(arguments)?),
            _ => return Err(ErrorData::invalid_params("unknown_tool", None)),
        };
        if !request.valid() {
            return Err(ErrorData::invalid_params("invalid_input", None));
        }
        #[cfg(any(unix, windows))]
        let result = crate::ipc::call(&self.socket, &self.credential, request).await;
        #[cfg(not(any(unix, windows)))]
        let result: Result<Response, Error> = {
            let _ = request;
            Err(Error::Unavailable)
        };
        match result {
            Ok(response) => {
                let value = match response {
                    Response::List(v) => serde_json::to_value(v),
                    Response::Summary(v) => serde_json::to_value(v),
                    Response::History(v) => serde_json::to_value(v),
                    Response::Policy(v) => serde_json::to_value(v),
                    Response::Proposal(v) => serde_json::to_value(v),
                }
                .map_err(|_| ErrorData::internal_error("serialization_failed", None))?;
                Ok(CallToolResult::structured(value))
            }
            Err(e) => Ok(CallToolResult::structured_error(
                serde_json::json!({"error": {"code": e.code()}}),
            )),
        }
    }
}
impl ServerHandler for Adapter {
    /// Negotiate the SDK lifecycle while advertising only tool capabilities.
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("tenshu-mcp", env!("CARGO_PKG_VERSION")))
    }
    /// Return public discovery metadata without granting wallet-read authority.
    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tools(),
            ..Default::default()
        })
    }
    /// Route an SDK call through the same validated, closed tool dispatcher.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        self.invoke(
            &request.name,
            Value::Object(request.arguments.unwrap_or_default()),
        )
        .await
        .map(Into::into)
    }
}
