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
        Self {
            link,
            render_dir,
            tool_router: Self::tool_router(),
        }
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
        let path = self
            .render_dir
            .join(format!("{stem}@{}x.{}", render.scale, render.format));

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&render.base64)
            .map_err(std::io::Error::other)?;
        std::fs::write(&path, bytes)?;
        Ok(path)
    }

    /// Write exported bytes next to the renders and return the path.
    fn save_export(&self, stem: &str, extension: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(&self.render_dir)?;
        // Node ids contain a colon, which is not a legal Windows filename.
        let path = self
            .render_dir
            .join(format!("{}.{extension}", stem.replace(':', "-")));
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
                Ok(CallToolResult::success(vec![ContentBlock::text(
                    payload.get(),
                )]))
            }
            Err(e) => Ok(CallToolResult::error(vec![ContentBlock::text(explain(
                method, &e,
            ))])),
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
                Err(e) => Ok(Err(CallToolResult::error(vec![ContentBlock::text(
                    format!(
                        "{method} returned a result this server could not read: {e}. \
                     The plugin is probably a different version than the server."
                    ),
                )]))),
            },
            Err(e) => Ok(Err(CallToolResult::error(vec![ContentBlock::text(
                explain(method, &e),
            )]))),
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

/// An SVG export. Small enough to read, but a real icon set is not, so it is
/// saved to disk as well as returned.
#[derive(Debug, Deserialize)]
struct SvgExport {
    #[serde(rename = "nodeId")]
    node_id: String,
    name: String,
    svg: String,
}

/// The original bytes behind an image fill.
#[derive(Debug, Deserialize)]
struct ImageBytes {
    hash: String,
    width: f64,
    height: f64,
    base64: String,
}

/// SVGs up to this size are also returned inline; larger ones only by path.
const INLINE_SVG_LIMIT: usize = 20_000;

/// File extension for image bytes, by magic number.
///
/// Figma keeps the original upload, so this can be any web image format, and a
/// wrong extension would make the file unopenable by the tools that need it.
fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        "jpg"
    } else if bytes.starts_with(b"GIF8") {
        "gif"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP" {
        "webp"
    } else if bytes.len() >= 12 && bytes[4..8] == *b"ftyp" && bytes[8..12] == *b"avif" {
        "avif"
    } else {
        "bin"
    }
}

/// Whether these bytes are a format Figma accepts as an image fill.
///
/// Checked by magic number rather than by file extension: a mislabelled file
/// would otherwise fail inside the plugin, where the error is far less clear.
fn is_supported_image(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0x89, b'P', b'N', b'G'])   // PNG
        || bytes.starts_with(&[0xFF, 0xD8])         // JPEG
        || bytes.starts_with(b"GIF8") // GIF
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

#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct FindNodesArgs {
    /// Search under this node (a page id works). Omit to search the current
    /// page; set `all_pages` to search the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    /// Search every page. Slow on a large file. Ignored when `root_id` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all_pages: Option<bool>,
    /// Keep only these node types, e.g. ["INSTANCE", "TEXT", "RECTANGLE"].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub types: Option<Vec<String>>,
    /// Layer name contains this text, ignoring case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A solid fill colour as #rrggbb. Alpha is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_color: Option<String>,
    /// A solid stroke colour as #rrggbb. Alpha is ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_color: Option<String>,
    /// A fill, stroke, text or effect style id, as returned by get_styles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style_id: Option<String>,
    /// A variable id bound to any field, fill, stroke or effect, as returned
    /// by get_variable_defs or by `variableId` on a fill in get_node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variable_id: Option<String>,
    /// Instances of this component, or of any variant in this component set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_id: Option<String>,
    /// Most matches to return; the total is always counted in full. Default
    /// 200. Use 0 to count uses without listing them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateImageArgs {
    /// Absolute path to a PNG or JPEG on this machine.
    pub path: String,
    /// Position on the current page, in Figma canvas coordinates.
    pub x: f64,
    pub y: f64,
    /// Displayed size. Defaults to the image's own pixel dimensions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<f64>,
    /// Layer name. Defaults to the file's own name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// What actually goes over the link: the file, read and encoded.
#[derive(Debug, Serialize)]
struct CreateImageWire {
    base64: String,
    x: f64,
    y: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<f64>,
    name: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CloneNodeArgs {
    /// Node to duplicate, e.g. "90:30".
    pub node_id: String,
    /// Page or frame to put the copy in, so it can land on another page. Omit
    /// to keep it beside the original. Position is kept relative to the new
    /// parent unless `x`/`y` are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Where to put the copy. Omit either to leave it on top of the original.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    /// Layer name for the copy. Defaults to Figma's own "name (Copy)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Levels of the copy's subtree to return. Defaults to 2, which is enough
    /// to address the copy's direct children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreatePageArgs {
    /// Name for the new page. Refused if a page already has this name.
    pub name: String,
    /// Position among the pages, from 0. Defaults to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SetTextArgs {
    /// The TEXT node to change.
    pub node_id: String,
    /// The new contents. May be empty.
    pub characters: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteNodesArgs {
    /// Ids to remove.
    pub node_ids: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExportSvgArgs {
    /// Node to export, e.g. an icon or logo frame.
    pub node_id: String,
    /// Convert text to outlines so the SVG renders without the font. Defaults
    /// to true; set false to keep text as live `<text>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outline_text: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ImageBytesArgs {
    /// An image hash, as `imageHash` on an IMAGE fill in get_node.
    pub hash: String,
}

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
        self.call("figma/getDocument", &args, DOCUMENT_TIMEOUT)
            .await
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
        self.call("figma/getSelection", &args, DEFAULT_TIMEOUT)
            .await
    }

    #[tool(
        description = "Find nodes by type, name, solid fill or stroke colour, \
                       style id, variable id or component, and count how many \
                       there are. Returns ids and layer paths, not subtrees; \
                       read an id with get_node. At least one filter is \
                       required. This is also the usage lookup: filter by \
                       style_id, variable_id or component_id to see where one \
                       is used, with limit 0 for just the count. Searches \
                       inside instances too, so overrides are found. Text \
                       with mixed styles matches only on its node-level style."
    )]
    async fn find_nodes(
        &self,
        Parameters(args): Parameters<FindNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/findNodes", &args, DOCUMENT_TIMEOUT).await
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

    #[tool(description = "Read the document's variable collections and their \
                       values across modes — design tokens, in Figma's terms.")]
    async fn get_variable_defs(
        &self,
        Parameters(args): Parameters<Empty>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/getVariableDefs", &args, DEFAULT_TIMEOUT)
            .await
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
        description = "Place an image file from this machine onto the current \
                       Figma page as a new layer. This modifies the document."
    )]
    async fn create_image(
        &self,
        Parameters(args): Parameters<CreateImageArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = std::path::Path::new(&args.path);

        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "Cannot read {}: {e}. The path must be absolute and on the \
                     machine running this server.",
                    args.path
                ))]));
            }
        };

        // Figma accepts PNG, JPEG and GIF as image fills. Refusing here gives a
        // better message than the plugin failing on bytes it cannot decode.
        if !is_supported_image(&bytes) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "{} is not a PNG, JPEG or GIF. Figma image fills accept only \
                 those formats.",
                args.path
            ))]));
        }

        let name = args.name.unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Image".into())
        });

        let wire = CreateImageWire {
            base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
            x: args.x,
            y: args.y,
            width: args.width,
            height: args.height,
            name,
        };

        debug!(path = %args.path, bytes = bytes.len(), "placing image");
        self.call("figma/createImage", &wire, DOCUMENT_TIMEOUT)
            .await
    }

    #[tool(description = "Duplicate a node with all its children, styles and \
                       effects intact, optionally at a new position. Returns \
                       the copy's subtree and its pageId — the copies have new ids. \
                       Pass parent_id to put the copy on another page or in \
                       another frame; the original is untouched. Use this \
                       rather than rebuilding a design from its parts. This \
                       modifies the document.")]
    async fn clone_node(
        &self,
        Parameters(args): Parameters<CloneNodeArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/cloneNode", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(
        description = "Add a page to the file and return its id. Does not switch \
                       to it. Put nodes on it with clone_node and parent_id. \
                       This modifies the document."
    )]
    async fn create_page(
        &self,
        Parameters(args): Parameters<CreatePageArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/createPage", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(
        description = "Replace the contents of a text node, keeping its font, \
                       size, colour and effects. Returns the previous text and \
                       the node's new size. This modifies the document."
    )]
    async fn set_text(
        &self,
        Parameters(args): Parameters<SetTextArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/setText", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Delete nodes by id. Ids that no longer exist are \
                       reported rather than failing the call. This modifies \
                       the document.")]
    async fn delete_nodes(
        &self,
        Parameters(args): Parameters<DeleteNodesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        self.call("figma/deleteNodes", &args, DEFAULT_TIMEOUT).await
    }

    #[tool(description = "Export a node as SVG, so icons and logos stay vector. \
                       The file is saved to disk and its path returned; small \
                       SVGs are also returned inline.")]
    async fn export_svg(
        &self,
        Parameters(args): Parameters<ExportSvgArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let export: SvgExport = match self
            .call_parsed("figma/exportSvg", &args, DEFAULT_TIMEOUT)
            .await?
        {
            Ok(export) => export,
            Err(failure) => return Ok(failure),
        };

        let stem = format!("{}-svg", export.node_id);
        let saved = self
            .save_export(&stem, "svg", export.svg.as_bytes())
            .inspect_err(|e| debug!(error = %e, "svg not saved"))
            .ok();

        let location = match &saved {
            Some(path) => format!(" — saved to {}", path.display()),
            None => String::new(),
        };
        let mut blocks = vec![ContentBlock::text(format!(
            "{} ({}) as SVG, {} bytes{location}",
            export.name,
            export.node_id,
            export.svg.len()
        ))];
        if export.svg.len() <= INLINE_SVG_LIMIT || saved.is_none() {
            blocks.push(ContentBlock::text(export.svg));
        }
        Ok(CallToolResult::success(blocks))
    }

    #[tool(description = "Read the original bytes of an image fill by its hash \
                       (imageHash in get_node). The file is saved to disk and \
                       its path returned, in the format it was uploaded in.")]
    async fn get_image_bytes(
        &self,
        Parameters(args): Parameters<ImageBytesArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let image: ImageBytes = match self
            .call_parsed("figma/getImageBytes", &args, DOCUMENT_TIMEOUT)
            .await?
        {
            Ok(image) => image,
            Err(failure) => return Ok(failure),
        };

        let bytes = match base64::engine::general_purpose::STANDARD.decode(&image.base64) {
            Ok(bytes) => bytes,
            Err(e) => {
                return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                    "The plugin returned image {} but its bytes could not be decoded: {e}",
                    image.hash
                ))]));
            }
        };

        let extension = image_extension(&bytes);
        let stem = format!("image-{}", image.hash);
        let text = match self.save_export(&stem, extension, &bytes) {
            Ok(path) => format!(
                "Image {} is {}x{} {extension}, {} bytes — saved to {}",
                image.hash,
                image.width,
                image.height,
                bytes.len(),
                path.display()
            ),
            Err(e) => format!(
                "Image {} is {}x{} {extension}, {} bytes, but it could not be saved: {e}",
                image.hash,
                image.width,
                image.height,
                bytes.len()
            ),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        description = "Render a node or the current page to a base64 PNG. Use \
                       when the visual result matters more than the structure."
    )]
    async fn get_screenshot(
        &self,
        Parameters(args): Parameters<ScreenshotArgs>,
    ) -> Result<CallToolResult, ErrorData> {
        let render: Render = match self
            .call_parsed("figma/getScreenshot", &args, DOCUMENT_TIMEOUT)
            .await?
        {
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
            None => format!(
                "{} ({}) rendered at {}x",
                render.name, render.node_id, render.scale
            ),
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
            "find_nodes",
            "export_svg",
            "get_image_bytes",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "{expected} missing from {names:?}"
            );
        }
        for expected in [
            "create_image",
            "clone_node",
            "delete_nodes",
            "set_text",
            "create_page",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "{expected} missing from {names:?}"
            );
        }
        assert_eq!(names.len(), 16, "eleven reads plus five writes: {names:?}");
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
        let msg = explain(
            "figma/getDocument",
            &ErrorObject::deadline_exceeded("x", 60),
        );
        assert!(msg.to_lowercase().contains("narrower"), "{msg}");
    }

    /// A one-pixel PNG, base64 encoded, so the test exercises a real decode.
    const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    #[test]
    fn a_render_is_written_where_the_caption_says() {
        let dir =
            std::env::temp_dir().join(format!("figma-mcp-render-test-{}", std::process::id()));
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
    fn find_nodes_sends_only_the_filters_given() {
        let json = serde_json::to_string(&FindNodesArgs {
            fill_color: Some("#f53f3f".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(json, r##"{"fill_color":"#f53f3f"}"##);
    }

    #[test]
    fn image_bytes_get_the_extension_their_format_needs() {
        assert_eq!(image_extension(&[0x89, b'P', b'N', b'G', 0, 0]), "png");
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
        assert_eq!(image_extension(b"GIF89a"), "gif");
        assert_eq!(image_extension(b"RIFF\0\0\0\0WEBPVP8 "), "webp");
        assert_eq!(image_extension(b"\0\0\0\x1cftypavif\0\0"), "avif");
        assert_eq!(image_extension(b"unknown"), "bin");
    }

    #[test]
    fn an_export_is_saved_under_a_filename_windows_accepts() {
        let dir = std::env::temp_dir().join(format!("figma-mcp-export-{}", std::process::id()));
        let server = FigmaServer::new(Link::new(), dir.clone());

        let path = server.save_export("12:34-svg", "svg", b"<svg/>").unwrap();

        assert!(!path.file_name().unwrap().to_string_lossy().contains(':'));
        assert_eq!(std::fs::read(&path).unwrap(), b"<svg/>");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clone_and_page_params_omit_what_was_not_given() {
        let clone = CloneNodeArgs {
            node_id: "1:1".into(),
            parent_id: Some("2:2".into()),
            x: None,
            y: None,
            name: None,
            depth: None,
        };
        assert_eq!(
            serde_json::to_string(&clone).unwrap(),
            r#"{"node_id":"1:1","parent_id":"2:2"}"#
        );
        let page = CreatePageArgs {
            name: "Options".into(),
            index: None,
        };
        assert_eq!(serde_json::to_string(&page).unwrap(), r#"{"name":"Options"}"#);
    }

    #[test]
    fn optional_params_are_omitted_not_nulled() {
        let json = serde_json::to_string(&DocumentArgs { depth: None }).unwrap();
        assert_eq!(json, "{}", "a null depth would override the host's default");
    }
}
