Every AI call the kernel makes now leaves through one send path on
`AiProviderService`: `chat_once` for a complete chat answer, with or without
tools, `chat_stream` for the chatbot, and `embed_with` (behind `embed`) for an
embedding. AI search's expand, summary and follow-up, the sentiment classifier,
the writing assistant (`/api/v1/ai/assist`), the streaming chatbot, the AI
Assistant's tool-calling turns, the kernel's own embedding of items and of
semantic filter queries, and a plugin's `ai-request` all go through it. Each send
checks the provider's base URL with `validate_base_url`, takes the configured
timeout from `ai_timeouts` for its plane (request or background) unless the
feature configures its own, as the AI Assistant does, and goes through the
per-operation circuit breaker. A failure comes back as an error whose text is
safe to show; the provider's own words reach the log and nothing else.

The base URL check is the security half of this. Only the embedding call and the
connection test re-checked the URL at send time; the chat, tool and plugin paths
connected to whatever the `ai_providers` row named. The admin form refuses a
private address, but a row can reach `site_config` without the form, and an IP
literal never passes through the hardened client's resolver, so a provider row
naming `http://10.0.0.5` or `http://127.0.0.1:6379` was sent the prompt and the
API key. Each send now refuses it before a byte leaves.

The timeouts that were hard-coded per path (15 s for expansion, 30 s for a
summary or follow-up, 60 s for the writing assistant, 120 s for the chatbot) are
gone: each takes the `ai_timeouts` value for its operation and provider, which an
operator can raise. A streaming answer is bounded at its start, connecting and
the provider's response headers, and after that by the stream parser's per-chunk
idle timeout, so a long answer is not cut off by a timeout sized for a one-shot
call.

There was a request builder and response parser for each path, five pairs in
all, and they had drifted: the search and assist parsers read only the first
Anthropic text block and threw the token counts away, so the writing assistant
recorded every call with no prompt tokens and its whole total as completion
tokens. It now records what the provider reported. One builder and one parser
remain, in `services/ai_tools.rs`, along with the stream parsers in
`services/ai_chat.rs`; the unused `stream_ai_response` in the search routes is
deleted. A plugin's request and response shapes are unchanged. A plugin that
sends more than one system message to an Anthropic provider now has them joined
with a blank line rather than a single newline, and an OpenAI-compatible response
with no choice in it is now `ERR_AI_PROVIDER_ERROR` rather than an empty answer.
