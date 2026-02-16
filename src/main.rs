use clap::{Parser, Subcommand, ValueEnum};
use slotforge::fixture::{run_benchmark, run_demo};
use slotforge::model::WaitlistPolicy;
use slotforge::report::{write_benchmark_report, write_demo_report};
use std::error::Error;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "slotforge",
    version,
    about = "Deterministic reservation and fair-waitlist engine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the end-to-end synthetic reservation scenario.
    Demo {
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
        #[arg(long, value_enum, default_value_t = PolicyArg::Fair)]
        policy: PolicyArg,
    },
    /// Compare FIFO and fair promotion on one fixed request stream.
    Benchmark {
        #[arg(long, value_name = "DIR")]
        output_dir: PathBuf,
    },
    /// Serve the local SQLite-backed booking API.
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 5204)]
        port: u16,
        #[arg(long, default_value = "runtime/slotforge")]
        runtime_dir: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum PolicyArg {
    Fifo,
    Fair,
}

impl From<PolicyArg> for WaitlistPolicy {
    fn from(value: PolicyArg) -> Self {
        match value {
            PolicyArg::Fifo => WaitlistPolicy::Fifo,
            PolicyArg::Fair => WaitlistPolicy::Fair,
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    let summary = match cli.command {
        Command::Demo { output_dir, policy } => {
            write_demo_report(&run_demo(policy.into())?, &output_dir)?
        }
        Command::Benchmark { output_dir } => {
            write_benchmark_report(&run_benchmark()?, &output_dir)?
        }
        Command::Serve {
            host,
            port,
            runtime_dir,
        } => {
            let listener = tokio::net::TcpListener::bind(format!("{host}:{port}")).await?;
            axum::serve(
                listener,
                slotforge::server::app(runtime_dir)?.into_make_service(),
            )
            .await?;
            return Ok(());
        }
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
