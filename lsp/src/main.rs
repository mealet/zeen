#![allow(unused)]

mod analysis;
mod backend;
mod completion;
mod diagnostics;
mod modules;
mod position;
mod semantic;

use backend::Backend;
use tower_lsp_server::{LspService, Server};

#[tokio::main]
async fn main() {
    if std::env::args()
        .skip(1)
        .any(|arg| arg == "-v" || arg == "--version")
    {
        println!("zeen-lsp {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
