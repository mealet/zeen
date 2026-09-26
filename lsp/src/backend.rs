use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, ls_types::*};

use crate::{diagnostics, semantic};

#[derive(Debug)]
pub struct Backend {
    client: Client,
    documents: Arc<RwLock<HashMap<Uri, String>>>,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn publish(&self, uri: Uri, text: String) {
        let diagnostics = diagnostics::check(&text);
        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }
}

impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                semantic_tokens_provider: Some(
                    SemanticTokensOptions {
                        legend: semantic::legend(),
                        full: Some(SemanticTokensFullOptions::Bool(true)),
                        ..Default::default()
                    }
                    .into(),
                ),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: env!("CARGO_PKG_NAME").to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "zeen-lsp ready")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let text = self
            .documents
            .read()
            .expect("RwLock guard error")
            .get(&params.text_document.uri)
            .cloned();

        let Some(text) = text else {
            return Ok(None);
        };

        Ok(Some(
            SemanticTokens {
                result_id: None,
                data: semantic::tokens_for(&text),
            }
            .into(),
        ))
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        self.documents
            .write()
            .expect("RwLock guard error")
            .insert(uri.clone(), text.clone());

        self.publish(uri, text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };

        let uri = params.text_document.uri;

        self.documents
            .write()
            .expect("RwLock guard error")
            .insert(uri.clone(), change.text.clone());

        self.publish(uri, change.text).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .expect("RwLock guard error")
            .remove(&params.text_document.uri);

        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await
    }
}
