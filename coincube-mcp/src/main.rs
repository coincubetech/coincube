use rmcp::{ServiceExt, service::RoleServer, transport::async_rw::AsyncRwTransport};
#[tokio::main]
async fn main() {
    if run().await.is_err() {
        eprintln!(
            "Tenshu MCP unavailable. Check the desktop grant and private client configuration."
        );
        std::process::exit(1);
    }
}
async fn run() -> Result<(), ()> {
    if !cfg!(unix) {
        return Err(());
    }
    let socket = std::env::var_os("TENSHU_MCP_SOCKET").ok_or(())?.into();
    let credential = std::env::var("TENSHU_MCP_CREDENTIAL").map_err(|_| ())?;
    if credential.len() != 64 || !credential.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(());
    }
    let adapter = coincube_mcp::adapter::Adapter { socket, credential };
    let transport =
        AsyncRwTransport::<RoleServer, _, _>::new(tokio::io::stdin(), tokio::io::stdout())
            .with_max_line_length(8192);
    let service = adapter.serve(transport).await.map_err(|_| ())?;
    service.waiting().await.map_err(|_| ())?;
    Ok(())
}
