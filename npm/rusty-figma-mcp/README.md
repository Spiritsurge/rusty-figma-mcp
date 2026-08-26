# rusty-figma-mcp

MCP server that reads and edits the Figma file you have open, through a plugin
running in your own session. No API token, no REST quotas.

```json
{
  "mcpServers": {
    "figma": { "command": "npx", "args": ["-y", "rusty-figma-mcp"] }
  }
}
```

You also need the Figma plugin, which is the other half of the bridge. See the
[project README](https://github.com/Spiritsurge/rusty-figma-mcp#setup).

Node is used only to fetch and launch the binary — the server itself is a static
executable with no runtime dependency. Prebuilt binaries ship as platform
packages under `optionalDependencies`, so npm installs exactly the one your
machine needs and nothing is downloaded at install time.
