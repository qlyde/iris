use clap::Parser;
use env_logger::Env;
use iris_lib::Iris;
use std::net::IpAddr;

#[derive(Parser)]
struct Arguments {
    #[clap(default_value = "127.0.0.1")]
    ip_address: IpAddr,

    #[clap(default_value = "6991")]
    port: u16,
}

fn main() {
    // init env_logger
    let env = Env::default().filter_or("RUST_LOG", "debug");
    env_logger::init_from_env(env);

    // start iris
    let arguments = Arguments::parse();
    Iris::new(arguments.ip_address, arguments.port).start();
}
