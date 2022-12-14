use clap::Parser;
use dotenv::dotenv;
use log::debug;

mod client;

#[derive(Parser, Debug)]
struct Cli {
    #[arg(short, long)]
    name: String,
    #[arg(short, long)]
    to_dial: Option<String>,
    #[arg(short, long)]
    listen_addr: Option<String>,
}

#[async_std::main]
async fn main() -> anyhow::Result<()> {
    dotenv().ok();
    env_logger::init();
    let cli = Cli::parse();
    debug!("Cli: {:?}", cli);

    let mut client = client::Client::new(
        &cli.name,
        cli.listen_addr.as_deref(),
        cli.to_dial.as_deref(),
    )
    .await?;

    client.start().await;

    Ok(())
}
