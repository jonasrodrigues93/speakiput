#[tokio::main]
async fn main() {
    if let Err(error) = speakiput_testing::conformance::run().await {
        eprintln!("conformance failed: {error}");
        std::process::exit(1);
    }
    println!("speakiput protocol conformance: ok");
}
