The AI assistant, AI chat, the writing assistant and AI search now work with
every current Anthropic model. Anthropic models newer than `claude-sonnet-4-6`
answer 400 to any request that carries `temperature`, and the kernel put one on
every request: the assistant and chat configs defaulted it to 0.2 and 0.7, so
it was never absent, and the writing assistant and search hard coded 0.7 and
0.3. Temperature is now optional end to end. The assistant and chat configs
hold it as unset by default, the admin forms take a blank field as unset, and a
request carries the key only when a site has set one. The writing assistant and
search have no setting for it and send none. A config stored with a number in
it still loads and still sends that number; nothing rewrites stored settings,
so a site that wants a current Claude model clears the field.

A provider call that fails now says why in the log. The tool calling path
turned a non-2xx answer into "the AI provider returned HTTP 400" and dropped
the body, which is where Anthropic names the parameter it refused, and search
logged nothing at all. Every outbound AI path (assistant and chat, streaming
chat, the writing assistant, search, embeddings and the plugin `ai-request`
call) now logs the status and the provider's response body, cut to 1,000
characters and with the request's key removed should a provider echo it. What
the person sees is unchanged.

The connection test fails on any non-2xx answer. The Anthropic test sent a GET
to the bare base URL and counted any HTTP answer as success, so a 404 read
"Connected successfully". Both protocols now ask `{base_url}/models`, which
needs a valid key and is not billed, and a failure shows the status and the
provider's explanation.

The assistant's `done` event reports the tokens the message spent. It was
built with zeros while the conversation added the real counts to its total; it
now carries the sum over every model call in the turn, the same calls and
totals the conversation records.

Apply after sending a message no longer fails with 403. CSRF tokens are single
use, and `assistant.js` sent the page's one token with the message and then the
same spent token with Apply. Every response to an assistant post (message,
apply, discard, start over) now returns a fresh token in an `X-CSRF-Token`
header, and the page script uses it for the next post and puts it into the
page's plain forms for their no-JavaScript fallback.
