use anyhow::{Context, Result, bail};
use clap::Parser;
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
};

/// Run NightBloom's local desktop HTTP service without opening a browser.
#[derive(Parser)]
struct Args {
    /// Data directory containing the sessions folder.
    #[arg(long)]
    data: PathBuf,
    /// Built frontend bundle directory.
    #[arg(long, default_value_os_t = PathBuf::from("frontend").join("dist"))]
    frontend: PathBuf,
    /// Loopback port (8765 is reserved for the owner's Python app).
    #[arg(long, default_value_t = 8876)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.port == 8765 {
        bail!("Port 8765 is reserved for the owner's Python app");
    }
    let (app, shutdown) = astroimp_desktop::server::router_with_shutdown(args.data, args.frontend)?;
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, args.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .context("Binding loopback HTTP server")?;
    eprintln!("NightBloom listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("Cannot listen for shutdown: {error}");
            }
            shutdown.cancel_and_wait();
        })
        .await
        .context("Serving NightBloom")
}
