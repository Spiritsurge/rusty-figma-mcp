//! The Figma binding: MCP tools mapped onto `figma/*` link methods.
//!
//! Every tool is the same three steps — serialize params, call the host, hand
//! back the payload untouched. The payload is never parsed here either (§7); it
//! goes to the MCP client as the bytes Figma produced.

use std::time::Duration;

use hostlink::{ErrorObject, Link, codes};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerInfo};
use rmcp::{ErrorData, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tracing::debug;

/// Default per-request deadline (§6).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Whole-document reads traverse the entire node tree and legitimately take
/// longer before the first progress notification arrives.
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
pub struct FigmaServer {
    link: Link,
    tool_router: ToolRouter<Self>,
}

impl FigmaServer {
    pub fn new(link: Link) -> Self {
        Self { link, tool_router: Self::tool_router() }
    }

    /// Call a host method and return its result verbatim.
    async fn call<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<CallToolResult, ErrorData> {
        let encoded = serde_json::to_string(params)
            .and_then(RawValue::from_string)
            .map_err(|e| ErrorData::internal_error(format!("encode params: {e}"), None))?;

        match self.link.request(method, Some(&encoded), timeout).await {
            Ok(payload) => {
                debug!(method, bytes = payload.len(), "ok");
                Ok(CallToolResult::success(vec![ContentBlock::text(payload.get())]))
            }
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(explain(method, &e))])),
        }
    }
}

/// Turn a protocol error into something an agent can act on.
///
/// These reach the model, not a log file, so each one says what to do next
/// rather than only what went wrong.
fn explain(method: &str, e: &ErrorObject) -> String {
    match e.code {
        codes::DISCONNECTED => format!(
            "No Figma plugin is connected. Open the file in Figma, run the \
             plugin, and pick this session in its list. ({method})"
        ),
        codes::DEADLINE_EXCEEDED => format!(
            "{method} timed out. The document may be very large — try a \
             narrower request, such as a specific node instead of the whole \
             document."
        ),
        codes::NOT_FOUND => format!("{}: {}", method, e.message),
        codes::METHOD_NOT_FOUND => format!(
            "The connected plugin does not implement {method}. It is probably \
             older than this server; update the plugin."
        ),
        _ => format!("{}: {} (code {})", method, e.message, e.code),
    }
}

// ---------------------------------------------------------------------------
// Parameter types
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct DocumentArgs {
    /// How many levels of the node tree to include. Omit for the whole tree,
    /// which on a large file can be very large indeed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct NodeArgs {
    /// Figma node id, as it appears in a file URL, e.g. "1:23".
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct Empty {}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ScreenshotArgs {
    /// Node to render. Omit to render the current page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Render scale; 1.0 is the design's own size. Defaults to 2.0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[tool_router]
impl FigmaServer {
    #[tool(
        description = "Read the structure of the open Figma document as a node \
                       tree. Prefer a depth limit or get_node on a specific \
                       frame; a whole large file can be tens of megabytes."
    )]
    async fn get_document(
        &self,
        Parameters(args): Parameters<DocumentArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getDocument", &args, DOCUMENT_TIMEOUT).await
    }

    #[tool(
        description = "Read one node and its subtree by id. This is the right \
                       tool for inspecting a specific frame or component."
    )]
    async fn get_node(
        &self,
        Parameters(args): Parameters<NodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getNode", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(
        description = "Read the nodes the user currently has selected in Figma. \
                       Use this when the user refers to 'this' or 'the selected \
                       frame'."
    )]
    async fn get_selection(
        &self,
        Parameters(args): Parameters<Empty>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getSelection", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(
        description = "Read the document's local paint, text, effect and grid \
                       styles — the design system's named values."
    )]
    async fn get_styles(
        &self,
        Parameters(args): Parameters<Empty>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getStyles", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(
        description = "Read the document's variable collections and their \
                       values across modes — design tokens, in Figma's terms."
    )]
    async fn get_variable_defs(
        &self,
        Parameters(args): Parameters<Empty>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getVariableDefs", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(description = "List the pages in the open document.")]
    async fn get_pages(
        &self,
        Parameters(args): Parameters<Empty>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getPages", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(
        description = "Read file name, current page and selection count. Cheap; \
                       useful as a first call to confirm which file is open."
    )]
    async fn get_metadata(
        &self,
        Parameters(args): Parameters<Empty>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getMetadata", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(
        description = "Render a node or the current page to a base64 PNG. Use \
                       when the visual result matters more than the structure."
    )]
    async fn get_screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getScreenshot", &args, DOCUMENT_TIMEOUT).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for FigmaServer {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo and Implementation are #[non_exhaustive], so they are
        // built by constructor and assignment rather than struct literal.
        let mut info = ServerInfo::default();
        info.server_info = Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Reads the Figma document currently open in the desktop or web app, \
             through a plugin the user runs and connects.\n\n\
             If a tool reports that no plugin is connected, tell the user to open \
             the plugin in Figma and select this session — it is not something \
             you can fix by retrying.\n\n\
             Start with get_metadata to confirm which file is open. Prefer \
             get_selection or get_node over get_document: whole-document reads on \
             a real file are very large."
                .into(),
        );
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tier_a_verb_is_registered() {
        let names: Vec<String> = FigmaServer::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();

        for expected in [
            "get_document",
            "get_node",
            "get_selection",
            "get_styles",
            "get_variable_defs",
            "get_pages",
            "get_metadata",
            "get_screenshot",
        ] {
            assert!(names.contains(&expected.to_string()), "{expected} missing from {names:?}");
        }
        assert_eq!(names.len(), 8, "Tier A is eight verbs: {names:?}");
    }

    #[test]
    fn every_tool_carries_a_description() {
        for tool in FigmaServer::tool_router().list_all() {
            let d = tool.description.as_deref().unwrap_or("");
            assert!(!d.is_empty(), "{} has no description", tool.name);
        }
    }

    /// A disconnected host is the most common failure by far, so its message
    /// has to tell the user what to do rather than merely what broke.
    #[test]
    fn disconnect_message_says_what_to_do() {
        let msg = explain("figma/getNode", &ErrorObject::disconnected());
        assert!(msg.contains("plugin"), "{msg}");
        assert!(msg.contains("Figma"), "{msg}");
    }

    #[test]
    fn timeout_message_suggests_narrowing() {
        let msg = explain("figma/getDocument", &ErrorObject::deadline_exceeded("x", 60));
        assert!(msg.to_lowercase().contains("narrower"), "{msg}");
    }

    #[test]
    fn optional_params_are_omitted_not_nulled() {
        let json = serde_json::to_string(&DocumentArgs { depth: None }).unwrap();
        assert_eq!(json, "{}", "a null depth would override the host's default");
    }
}
