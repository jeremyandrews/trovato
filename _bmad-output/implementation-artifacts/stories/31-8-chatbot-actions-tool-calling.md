# Story 31.8: Chatbot Actions (Tool Calling)

Status: ready-for-dev

## Story

As a **site visitor using the chatbot**,
I want the AI to perform actions like searching conferences or looking up details,
so that the chatbot can provide precise, data-driven answers rather than only generating text.

## Acceptance Criteria

1. **AC1: tap_chat_actions Registration** — `tap_chat_actions` is listed in `KNOWN_TAPS` and plugins can register chatbot actions by implementing this tap. Each action declares a name, description, and parameter schema (JSON Schema format compatible with LLM function-calling).

2. **AC2: Action Discovery** — On chatbot initialization or first request, the kernel dispatches `tap_chat_actions` to all enabled plugins and collects registered actions. Actions are formatted as function-calling tool definitions for the AI provider.

3. **AC3: trovato_ai Plugin Actions** — The `trovato_ai` plugin registers three domain-specific actions via `tap_chat_actions`:
   - `search_conferences` — Search by keyword, topic, or location
   - `get_conference_details` — Get full details by conference ID
   - `list_upcoming_cfps` — List conferences with open CFPs

4. **AC4: Tool Call Dispatch** — When the AI provider responds with a tool call (function call), the kernel identifies the target plugin and action, deserializes parameters, and dispatches the call to the plugin's action handler. The result is sent back to the AI as a tool response for final answer generation.

5. **AC5: Multi-Turn Tool Calling** — The chatbot supports multi-turn tool calling: AI requests a tool call -> kernel executes -> result returned to AI -> AI generates final response (or requests another tool call). Maximum tool call rounds configurable (default 3) to prevent infinite loops.

6. **AC6: Action Permission Gating** — Actions respect the calling user's permissions. The user context is passed through the tool dispatch so actions can check `access content` or other permissions before executing.

7. **AC7: Error Handling** — If an action fails (database error, permission denied, invalid parameters), a structured error message is returned to the AI as the tool response so it can inform the user gracefully.

8. **AC8: Integration Tests** — Tests verify: (a) `tap_chat_actions` returns valid action definitions; (b) action parameter schemas are valid JSON Schema; (c) tool call dispatch routes to correct plugin; (d) max tool call rounds limit is enforced; (e) action failures produce structured error responses.

## Tasks / Subtasks

- [x] Task 1: Add `tap_chat_actions` to KNOWN_TAPS (AC: #1)
  - [x] 1.1 `"tap_chat_actions"` already in `KNOWN_TAPS` (line 125 of `info_parser.rs`)
- [x] Task 2: Scaffold `tap_chat_actions` in trovato_ai plugin (AC: #3)
  - [x] 2.1 `tap_chat_actions` handler exists in `plugins/trovato_ai/src/lib.rs` returning 3 action definitions (lines 109-137)
- [ ] Task 3: Implement action discovery in kernel (AC: #2)
  - [ ] 3.1 Add `collect_chat_actions()` method to `ChatService` or plugin dispatcher
  - [ ] 3.2 Dispatch `tap_chat_actions` to all enabled plugins
  - [ ] 3.3 Parse and validate returned action definitions
  - [ ] 3.4 Format actions as provider-specific tool definitions (OpenAI function-calling or Anthropic tool-use format)
- [ ] Task 4: Implement tool call dispatch (AC: #4, #5)
  - [ ] 4.1 Detect tool call in provider streaming response
  - [ ] 4.2 Route tool call to target plugin's action handler
  - [ ] 4.3 Execute action with user context
  - [ ] 4.4 Return result to AI as tool response message
  - [ ] 4.5 Implement multi-turn loop with configurable max rounds
- [ ] Task 5: Implement action handlers in trovato_ai (AC: #3)
  - [ ] 5.1 `search_conferences` — call SearchService or DB query
  - [ ] 5.2 `get_conference_details` — load item by ID
  - [ ] 5.3 `list_upcoming_cfps` — query items with CFP date filter
- [ ] Task 6: Permission gating and error handling (AC: #6, #7)
  - [ ] 6.1 Pass UserContext through action dispatch
  - [ ] 6.2 Check permissions in action handlers
  - [ ] 6.3 Wrap action execution in error handling, return structured errors
- [ ] Task 7: Integration tests (AC: #8)

## Dev Notes

### Current State

The infrastructure for chatbot actions exists:
- `tap_chat_actions` is in `KNOWN_TAPS` (line 125 of `info_parser.rs`)
- `trovato_ai` plugin registers 3 actions with full name, description, and parameter schemas (lines 109-137 of `plugins/trovato_ai/src/lib.rs`)
- The actions are returned as JSON with an `"actions"` array

What remains:
- Kernel-side action discovery (dispatching `tap_chat_actions` and collecting results)
- Tool call detection in streaming responses from both OpenAI and Anthropic protocols
- Tool call dispatch routing to the correct plugin's action handler
- Multi-turn tool calling loop with round limiting
- Actual action handler implementations in trovato_ai (search, get details, list CFPs)
- Integration with `ChatService` streaming pipeline in `ai_chat.rs`

### Action Definition Format

```json
{
  "actions": [
    {
      "name": "search_conferences",
      "description": "Search for conferences by keyword, topic, or location",
      "parameters": {
        "query": {"type": "string", "description": "Search query"},
        "topic": {"type": "string", "description": "Topic slug (optional)"},
        "country": {"type": "string", "description": "Country name (optional)"}
      }
    }
  ]
}
```

### Provider Tool Calling Formats

**OpenAI function-calling:**
```json
{
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "search_conferences",
        "description": "...",
        "parameters": { "type": "object", "properties": {...} }
      }
    }
  ]
}
```

**Anthropic tool-use:**
```json
{
  "tools": [
    {
      "name": "search_conferences",
      "description": "...",
      "input_schema": { "type": "object", "properties": {...} }
    }
  ]
}
```

### Key Files

- `crates/kernel/src/plugin/info_parser.rs` — KNOWN_TAPS (tap_chat_actions at line 125)
- `plugins/trovato_ai/src/lib.rs` — tap_chat_actions handler (lines 109-137)
- `crates/kernel/src/services/ai_chat.rs` — ChatService (streaming pipeline to extend)
- `crates/kernel/src/routes/api_chat.rs` — SSE endpoint (may need tool call handling)
