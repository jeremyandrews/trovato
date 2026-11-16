Every outbound AI provider request now goes through the kernel's SSRF-hardened
HTTP client. This covers chat, streaming chat, tool calling chat, embeddings,
the connection test and the plugin `ai-request` host call. The AI provider
service built its own plain `reqwest` client instead of using the shared
`hardened_outbound_builder()`. So AI requests skipped two checks that every
other outbound path has: the resolve-time address check and the per-hop
redirect revalidation. A provider host name that resolved to a private address
was connected to, and a provider redirect to one was followed. The client build
also fell back to an unhardened default client if it failed. The service now
builds its client from the shared builder, keeps its default request timeout,
and refuses to start if the client cannot be built.

Upgrade note: a provider whose host name resolves to a private, loopback,
link-local or other reserved address is now refused when the request connects.
For example, `ollama.local` or `ollama.internal` pointing at a LAN or
local machine was reachable before and is refused now. Local and LAN model
servers are not a supported provider address. The admin form's checks on the
configured base URL are unchanged.
