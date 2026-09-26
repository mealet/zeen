use std::{
    collections::HashMap,
    sync::Arc, time::Duration,
};

use tower_lsp_server::{Client, LanguageServer, jsonrpc::Result, ls_types::*};
use tokio::sync::RwLock;

use crate::{diagnostics, semantic};

const DIAGNOSTIC_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct Backend {
    client: Client,
    documents: Arc<RwLock<HashMap<Uri, Document>>>,
}

#[derive(Debug, Clone)]
struct Document {
    version: i32,
    text: String,
}

impl Backend {
    pub fn new(client: Client) -> Self {
        Self {
            client,
            documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn publish(&self, uri: Uri, text: String) {
        let diagnostics = diagnostics::check(&uri, &text);
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
                        range: Some(true),
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
            .await
            .get(&params.text_document.uri)
            .map(|document| document.text.clone());

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

    async fn semantic_tokens_range(
        &self,
        params: SemanticTokensRangeParams,
    ) -> Result<Option<SemanticTokensRangeResult>> {
        let text = self
            .documents
            .read()
            .await
            .get(&params.text_document.uri)
            .map(|document| document.text.clone());

        let Some(text) = text else {
            return Ok(None);
        };

        Ok(Some(
            SemanticTokens {
                result_id: None,
                data: semantic::tokens_in_range(
                    &text,
                    params.range.start.line,
                    params.range.end.line,
                ),
            }
            .into(),
        ))
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let document = Document {
            version: params.text_document.version,
            text: params.text_document.text,
        };

        self.documents
            .write()
            .await
            .insert(uri.clone(), document.clone());

        self.publish(uri, document.text).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let Some(change) = params.content_changes.into_iter().last() else {
            return;
        };

        let uri = params.text_document.uri;
        let document = Document {
            version: params.text_document.version,
            text: change.text,
        };

        self.documents
            .write()
            .await
            .insert(uri.clone(), document);

        let client = self.client.clone();
        let documents = Arc::clone(&self.documents);
        let version = params.text_document.version;

        tokio::spawn(async move {
            tokio::time::sleep(DIAGNOSTIC_DEBOUNCE).await;

            let current = documents.read().await.get(&uri).cloned();

            let Some(current) = current else {
                return;
            };

            if current.version != version {
                return;
            }

            let diagnostics = diagnostics::check(&uri, &current.text);
            client.publish_diagnostics(uri, diagnostics, None).await;
        });
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .write()
            .await
            .remove(&params.text_document.uri);

        self.client
            .publish_diagnostics(params.text_document.uri, Vec::new(), None)
            .await
    }
}
