//! The Figma binding: MCP tools mapped onto `figma/*` link methods.
//!
//! Every tool is the same three steps — serialize params, call the host, hand
//! back the payload untouched. The payload is not parsed here either (§7); it
//! goes to the MCP client as the bytes Figma produced.
//!
//! `get_screenshot` is the single exception, and a deliberate one: its base64
//! has to be lifted out of the envelope to become an MCP image block. See
//! [`Render`].

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
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
    render_dir: PathBuf,
    tool_router: ToolRouter<Self>,
}

impl FigmaServer {
    pub fn new(link: Link, render_dir: PathBuf) -> Self {
        Self { link, render_dir, tool_router: Self::tool_router() }
    }

    /// Write a render to disk and return its path.
    ///
    /// A model can look at an image block but cannot open it, edit it, or hand
    /// it to another tool. Saving the file as well means a render can actually
    /// be used — composited, cropped, attached — instead of only viewed.
    fn save_render(&self, render: &Render) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.render_dir)?;

        // Node ids contain a colon, which is not a legal Windows filename.
        let stem = render.node_id.replace(':', "-");
        let path = self.render_dir.join(format!("{stem}@{}x.{}", render.scale, render.format));

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&render.base64)
            .map_err(std::io::Error::other)?;
        std::fs::write(&path, bytes)?;
        Ok(path)
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

    /// Call a host method whose result needs restructuring before it reaches
    /// the client, rather than being forwarded verbatim.
    async fn call_parsed<P: Serialize, T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<Result<T, CallToolResult>, ErrorData> {
        let encoded = serde_json::to_string(params)
            .and_then(RawValue::from_string)
            .map_err(|e| ErrorData::internal_error(format!("encode params: {e}"), None))?;

        match self.link.request(method, Some(&encoded), timeout).await {
            Ok(payload) => match serde_json::from_str::<T>(payload.get()) {
                Ok(value) => Ok(Ok(value)),
                Err(e) => Ok(Err(CallToolResult::error(vec![ContentBlock::text(format!(
                    "{method} returned a result this server could not read: {e}. \
                     The plugin is probably a different version than the server."
                ))]))),
            },
            Err(e) => Ok(Err(CallToolResult::error(vec![ContentBlock::text(explain(method, &e))]))),
        }
    }
}

/// What the host returns for a render.
///
/// Screenshots are the one place a payload is parsed rather than forwarded
/// (§7). The rule exists so that document reads, which are unbounded, never
/// become object graphs; a render is bounded by the image itself, and its
/// base64 has to be lifted out of the envelope regardless to become an MCP
/// image block. Forwarding it as JSON text would hand the model a wall of
/// base64 it cannot look at, which defeats the point of the tool.
#[derive(Debug, Deserialize)]
struct Render {
    #[serde(rename = "nodeId")]
    node_id: String,
    name: String,
    format: String,
    scale: f32,
    base64: String,
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
        let render: Render =
            match self.call_parsed("figma/getScreenshot", &args, DOCUMENT_TIMEOUT).await? {
                Ok(render) => render,
                Err(failure) => return Ok(failure),
            };

        debug!(node = %render.node_id, bytes = render.base64.len(), "rendered");

        let saved = self
            .save_render(&render)
            .inspect_err(|e| debug!(error = %e, "render not saved"))
            .ok();

        let caption = match &saved {
            Some(path) => format!(
                "{} ({}) rendered at {}x — saved to {}",
                render.name,
                render.node_id,
                render.scale,
                path.display()
            ),
            None => format!("{} ({}) rendered at {}x", render.name, render.node_id, render.scale),
        };

        // The image block first: clients render it, and a vision model can
        // actually see it. The caption carries what the image cannot say,
        // including where the file landed.
        Ok(CallToolResult::success(vec![
            ContentBlock::image(render.base64, format!("image/{}", render.format)),
            ContentBlock::text(caption),
        ]))
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

    /// A one-pixel PNG, base64 encoded, so the test exercises a real decode.
    const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    #[test]
    fn a_render_is_written_where_the_caption_says() {
        let dir = std::env::temp_dir().join(format!("figma-mcp-render-test-{}", std::process::id()));
        let server = FigmaServer::new(Link::new(), dir.clone());

        let render = Render {
            node_id: "90:30".into(),
            name: "Banner".into(),
            format: "png".into(),
            scale: 2.0,
            base64: PIXEL.into(),
        };

        let path = server.save_render(&render).expect("saved");

        // A colon is legal in a Figma node id and illegal in a Windows
        // filename, so it has to be replaced rather than passed through.
        assert!(!path.file_name().unwrap().to_string_lossy().contains(':'));
        assert!(path.to_string_lossy().contains("90-30"));

        let written = std::fs::read(&path).expect("readable");
        assert_eq!(
            &written[..8],
            &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
            "should be a real PNG"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_malformed_render_does_not_write_a_file() {
        let dir = std::env::temp_dir().join(format!("figma-mcp-bad-render-{}", std::process::id()));
        let server = FigmaServer::new(Link::new(), dir.clone());

        let render = Render {
            node_id: "1:1".into(),
            name: "Bad".into(),
            format: "png".into(),
            scale: 1.0,
            base64: "not valid base64 !!!".into(),
        };

        assert!(server.save_render(&render).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn optional_params_are_omitted_not_nulled() {
        let json = serde_json::to_string(&DocumentArgs { depth: None }).unwrap();
        assert_eq!(json, "{}", "a null depth would override the host's default");
    }
}
