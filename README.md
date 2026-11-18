# tackle

mcp-name: ai.qompass/tackle

Tackle — live game-editor broker as an MCP stdio server (U1 read core,
U2 write core). Tackle brokers a running game editor to MCP clients:
the editor-side package connects to the server's local bridge
(`ws://127.0.0.1:9770/editor`), and the server exposes the editor's
scene and objects to agents over stdio through the Model Context
Protocol — read tools in the U1 core, write tools in the U2 core.

The binary is named `tackle`. Published on the MCP Registry as
`ai.qompass/tackle`.
