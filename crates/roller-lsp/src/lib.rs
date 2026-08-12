//! Language Server Protocol support for Roller build scripts.

mod analysis;
mod protocol;

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::PathBuf;

use analysis::AnalysisContext;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
struct Document {
    text: String,
    version: Option<i64>,
}

/// Stateful Roller LSP request handler.
#[derive(Debug, Default)]
pub struct Server {
    documents: HashMap<String, Document>,
    root: Option<PathBuf>,
    shutdown_requested: bool,
    exit_requested: bool,
}

impl Server {
    /// Handle one decoded JSON-RPC message.
    #[must_use]
    pub fn handle(&mut self, message: Value) -> Vec<Value> {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Vec::new();
        };
        let id = message.get("id").cloned();
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        match method {
            "initialize" => {
                self.set_root(&params);
                id.map(|id| success(id, initialize_result()))
                    .into_iter()
                    .collect()
            }
            "initialized" => vec![notification(
                "window/logMessage",
                json!({"type": 3, "message": "Roller language server initialized"}),
            )],
            "shutdown" => {
                self.shutdown_requested = true;
                id.map(|id| success(id, Value::Null)).into_iter().collect()
            }
            "exit" => {
                self.exit_requested = true;
                Vec::new()
            }
            "textDocument/didOpen" => self.did_open(&params).into_iter().collect(),
            "textDocument/didChange" => self.did_change(&params).into_iter().collect(),
            "textDocument/didSave" => self.did_save(&params).into_iter().collect(),
            "textDocument/didClose" => self.did_close(&params).into_iter().collect(),
            "textDocument/completion" => self.request(id, &params, |context, line, character| {
                analysis::completions(context, line, character)
            }),
            "textDocument/hover" => self.request(id, &params, |context, line, character| {
                analysis::hover(context, line, character)
            }),
            "textDocument/definition" => self.request(id, &params, |context, line, character| {
                analysis::definitions(context, line, character)
            }),
            "textDocument/documentSymbol" => {
                let result = document_uri(&params)
                    .and_then(|uri| self.with_context(uri, analysis::document_symbols))
                    .unwrap_or_else(|| json!([]));
                id.map(|id| success(id, result)).into_iter().collect()
            }
            "$/cancelRequest" | "$/setTrace" => Vec::new(),
            _ => id
                .map(|id| error_response(id, -32601, &format!("method not found: {method}")))
                .into_iter()
                .collect(),
        }
    }

    /// Whether an `exit` notification has been received.
    #[must_use]
    pub fn should_exit(&self) -> bool {
        self.exit_requested
    }

    fn set_root(&mut self, params: &Value) {
        self.root = params
            .get("rootUri")
            .and_then(Value::as_str)
            .and_then(file_uri_to_path)
            .or_else(|| {
                params
                    .get("rootPath")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            })
            .or_else(|| {
                params
                    .get("workspaceFolders")
                    .and_then(Value::as_array)
                    .and_then(|folders| folders.first())
                    .and_then(|folder| folder.get("uri"))
                    .and_then(Value::as_str)
                    .and_then(file_uri_to_path)
            });
    }

    fn did_open(&mut self, params: &Value) -> Option<Value> {
        let text_document = params.get("textDocument")?;
        let uri = text_document.get("uri")?.as_str()?.to_string();
        let text = text_document.get("text")?.as_str()?.to_string();
        let version = text_document.get("version").and_then(Value::as_i64);
        self.documents
            .insert(uri.clone(), Document { text, version });
        self.publish_diagnostics(&uri)
    }

    fn did_change(&mut self, params: &Value) -> Option<Value> {
        let text_document = params.get("textDocument")?;
        let uri = text_document.get("uri")?.as_str()?.to_string();
        let version = text_document.get("version").and_then(Value::as_i64);
        let text = params
            .get("contentChanges")?
            .as_array()?
            .last()?
            .get("text")?
            .as_str()?
            .to_string();
        self.documents
            .insert(uri.clone(), Document { text, version });
        self.publish_diagnostics(&uri)
    }

    fn did_save(&mut self, params: &Value) -> Option<Value> {
        let uri = document_uri(params)?.to_string();
        if let Some(text) = params.get("text").and_then(Value::as_str)
            && let Some(document) = self.documents.get_mut(&uri)
        {
            document.text = text.to_string();
        }
        self.publish_diagnostics(&uri)
    }

    fn did_close(&mut self, params: &Value) -> Option<Value> {
        let uri = document_uri(params)?.to_string();
        self.documents.remove(&uri);
        Some(notification(
            "textDocument/publishDiagnostics",
            json!({"uri": uri, "diagnostics": []}),
        ))
    }

    fn publish_diagnostics(&self, uri: &str) -> Option<Value> {
        let document = self.documents.get(uri)?;
        let path = file_uri_to_path(uri);
        let context = AnalysisContext {
            uri,
            source: &document.text,
            path: path.as_deref(),
            root: self.root.as_deref(),
        };
        let mut params = json!({
            "uri": uri,
            "diagnostics": analysis::diagnostics(&context),
        });
        if let Some(version) = document.version {
            params["version"] = json!(version);
        }
        Some(notification("textDocument/publishDiagnostics", params))
    }

    fn request(
        &self,
        id: Option<Value>,
        params: &Value,
        operation: impl FnOnce(&AnalysisContext<'_>, u64, u64) -> Value,
    ) -> Vec<Value> {
        let Some(id) = id else {
            return Vec::new();
        };
        let Some(uri) = document_uri(params) else {
            return vec![error_response(id, -32602, "missing text document URI")];
        };
        let Some((line, character)) = position(params) else {
            return vec![error_response(id, -32602, "missing text document position")];
        };
        let result = self
            .with_context(uri, |context| operation(context, line, character))
            .unwrap_or(Value::Null);
        vec![success(id, result)]
    }

    fn with_context<T>(
        &self,
        uri: &str,
        operation: impl FnOnce(&AnalysisContext<'_>) -> T,
    ) -> Option<T> {
        let text = self.document_text(uri)?;
        let path = file_uri_to_path(uri);
        let context = AnalysisContext {
            uri,
            source: &text,
            path: path.as_deref(),
            root: self.root.as_deref(),
        };
        Some(operation(&context))
    }

    fn document_text(&self, uri: &str) -> Option<String> {
        self.documents
            .get(uri)
            .map(|document| document.text.clone())
            .or_else(|| file_uri_to_path(uri).and_then(|path| std::fs::read_to_string(path).ok()))
    }
}

/// Run the Roller language server over stdin/stdout.
pub fn run_stdio() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();
    let mut server = Server::default();

    while let Some(message) = protocol::read_message(&mut reader)? {
        for response in server.handle(message) {
            protocol::write_message(&mut writer, &response)?;
        }
        if server.should_exit() {
            break;
        }
    }
    Ok(())
}

fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "positionEncoding": "utf-16",
            "textDocumentSync": {
                "openClose": true,
                "change": 1,
                "save": {"includeText": true},
            },
            "completionProvider": {
                "resolveProvider": false,
                "triggerCharacters": [".", ":", "\""],
            },
            "hoverProvider": true,
            "definitionProvider": true,
            "documentSymbolProvider": true,
        },
        "serverInfo": {
            "name": "roller-lsp",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

fn document_uri(params: &Value) -> Option<&str> {
    params.get("textDocument")?.get("uri")?.as_str()
}

fn position(params: &Value) -> Option<(u64, u64)> {
    let position = params.get("position")?;
    Some((
        position.get("line")?.as_u64()?,
        position.get("character")?.as_u64()?,
    ))
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex(bytes[index + 1])?;
            let low = hex(bytes[index + 2])?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(decoded).ok()?))
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_advertises_language_features() {
        let response = Server::default().handle(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"rootUri": "file:///tmp/project"},
        }));
        let capabilities = &response[0]["result"]["capabilities"];
        assert_eq!(capabilities["hoverProvider"], true);
        assert_eq!(capabilities["definitionProvider"], true);
        assert!(capabilities["completionProvider"].is_object());
    }

    #[test]
    fn open_and_change_publish_versioned_diagnostics() {
        let mut server = Server::default();
        let opened = server.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {
                "uri": "file:///tmp/build.roller",
                "languageId": "roller",
                "version": 1,
                "text": "section build() { let x = 1 }"
            }}
        }));
        assert_eq!(opened[0]["params"]["version"], 1);
        assert_eq!(opened[0]["params"]["diagnostics"][0]["code"], "parser");

        let changed = server.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didChange",
            "params": {
                "textDocument": {"uri": "file:///tmp/build.roller", "version": 2},
                "contentChanges": [{"text": "section build() {}"}]
            }
        }));
        assert_eq!(changed[0]["params"]["version"], 2);
        assert!(
            changed[0]["params"]["diagnostics"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn completion_request_uses_the_open_document() {
        let mut server = Server::default();
        let _ = server.handle(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/didOpen",
            "params": {"textDocument": {
                "uri": "file:///tmp/build.roller",
                "version": 1,
                "text": "import \"zig\"\nsection build() { zig:: }"
            }}
        }));
        let source_line = "section build() { zig:: }";
        let character = source_line.find("zig::").unwrap() + "zig::".len();
        let response = server.handle(json!({
            "jsonrpc": "2.0",
            "id": "completion",
            "method": "textDocument/completion",
            "params": {
                "textDocument": {"uri": "file:///tmp/build.roller"},
                "position": {"line": 1, "character": character}
            }
        }));
        let labels = response[0]["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect::<Vec<_>>();
        assert!(labels.contains(&"get_compiler"));
    }

    #[test]
    fn unknown_requests_receive_method_not_found() {
        let response = Server::default().handle(json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "roller/unknown",
        }));
        assert_eq!(response[0]["error"]["code"], -32601);
    }

    #[test]
    fn file_uri_decodes_spaces_and_unicode() {
        assert_eq!(
            file_uri_to_path("file:///tmp/a%20b/%E6%97%A5.roller"),
            Some(PathBuf::from("/tmp/a b/日.roller"))
        );
    }
}
