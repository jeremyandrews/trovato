# Security Analysis: Full-Serialization vs Handle-Based Field Access

**Date**: 2026-02-12
**Status**: Analysis for Phase 2 Architecture Decision

## Executive Summary

Full-serialization exposes ALL item fields to plugins. This document analyzes whether this breaks access control and what mitigations exist.

**Conclusion**: Full-serialization does NOT fundamentally break access control, but changes WHERE enforcement happens. Field-level restrictions shift from "plugin can't see the data" to "plugin sees data but Kernel rejects unauthorized modifications."

---

## The Access Control Question

### Handle-Based Model
```
Plugin calls: get_field_string(handle, "field_ssn")
Host checks: Does this plugin have permission to read field_ssn?
If no: Returns None/Error
If yes: Returns value
```
**Access control**: Enforced at read time. Plugin never sees restricted data.

### Full-Serialization Model
```
Plugin receives: { "field_ssn": "123-45-6789", "field_name": "John", ... }
Plugin can: Read any field in the payload
```
**Access control**: Plugin sees everything. Enforcement must happen elsewhere.

---

## Analysis: Does This Break Security?

### What We're Protecting Against

| Threat | Handle-Based | Full-Serialization |
|--------|--------------|-------------------|
| **Malicious plugin reads sensitive field** | Blocked at host call | Plugin sees data |
| **Malicious plugin modifies restricted field** | Blocked at host call | Blocked at Kernel validation |
| **Plugin leaks data via side-channel** | Can't access data to leak | Data available to leak |
| **Plugin logs/exfiltrates sensitive data** | Can't access data | Data available |

### The Critical Question: What Can Plugins Do?

WASM plugins in Trovato are **sandboxed**:
- No filesystem access
- No network access
- No environment variables
- Only host functions provided by Kernel

**A plugin that receives sensitive data cannot exfiltrate it** because it has no output channels except:
1. Return values (validated by Kernel)
2. Host function calls (controlled by Kernel)
3. Database writes (validated by Kernel)

### Real Attack Vectors

1. **Plugin returns sensitive data in render output**
   - Mitigation: Kernel validates render elements, strips unauthorized fields
   - Example: If `field_ssn` is restricted, Kernel removes it from any HTML/JSON output

2. **Plugin writes sensitive data to database**
   - Mitigation: Kernel validates all database writes against permissions
   - Example: Plugin can't INSERT user data into public tables

3. **Plugin uses host logging to exfiltrate**
   - Mitigation: Don't include sensitive data in log messages
   - Example: `log("Debug: {}", item.fields)` should sanitize

4. **Plugin stores data in WASM memory for later**
   - Reality: Store is dropped after request, no persistence
   - Mitigation: None needed - no cross-request memory

---

## Architecture Implications

### Option A: Accept Full-Serialization with Output Validation

**How it works**:
1. Plugin receives full item JSON
2. Plugin processes freely
3. Kernel validates ALL outputs before persistence/rendering
4. Restricted fields stripped from render output
5. Restricted field modifications rejected

**Pros**:
- Simpler plugin development
- Better performance (proven by benchmarks)
- Validation is centralized in Kernel

**Cons**:
- Plugins "see" sensitive data (even if they can't use it)
- Requires comprehensive output validation
- Audit logging more complex (plugin accessed field vs used field)

### Option B: Filtered Serialization (Hybrid)

**How it works**:
1. Plugin declares field dependencies in `.info.toml`
2. Kernel serializes ONLY declared fields
3. Undeclared fields not included in payload
4. Plugin can't access what it doesn't receive

```toml
[taps.options]
tap_item_view = {
  data_mode = "full",
  fields = ["title", "field_body", "field_summary"]
}
```

**Pros**:
- Plugins only receive what they need
- Reduced attack surface
- Smaller payloads (performance bonus for large items)
- Clear field dependency documentation

**Cons**:
- More configuration burden
- Plugin must declare dependencies accurately
- Doesn't help with dynamic field access

### Option C: Handle-Based Default, Full-Serialization Opt-In

**How it works**:
1. Default to handle-based (existing design)
2. Full-serialization requires explicit permission
3. `full_serialization_trusted` permission grants access

**Pros**:
- Maximum security by default
- Full-serialization only for trusted plugins

**Cons**:
- Performance penalty for common case (benchmarks show handle-based is slower)
- Ignores Phase 0 findings

---

## Recommendation

**Option B (Filtered Serialization)** provides the best balance:

1. **Performance**: Single boundary crossing (proven faster)
2. **Security**: Plugins only receive declared fields
3. **Simplicity**: Plugin development remains straightforward
4. **Auditability**: Field access is explicit in config

### Implementation Approach

```toml
# Plugin declares what it needs
[taps.options.tap_item_view]
fields = ["title", "field_body", "field_summary", "field_tags"]
# OR
fields = "*"  # Full access - requires trust level
```

**Kernel behavior**:
1. Load plugin field declarations at startup
2. Before serialization, filter item to declared fields only
3. Undeclared fields are absent, not null
4. `fields = "*"` requires plugin to have `full_field_access` permission

### Deferred Complexity

The following can be added later if needed:
- Per-field permission checks (e.g., `field_ssn` requires `view sensitive data`)
- Role-based field filtering (admin sees more fields than anonymous)
- Audit logging of field access patterns

---

## Questions for Stakeholder Review

1. **Do we need audit logging of which fields plugins access?**
   - If yes: Filtered serialization makes this trivial (check config)
   - If no: Full-serialization with output validation is sufficient

2. **Are there truly sensitive fields that plugins should never see?**
   - Example: User passwords, SSNs, payment tokens
   - If yes: These should be excluded from item serialization entirely

3. **What trust level do we assume for plugins?**
   - Core plugins: Fully trusted (can use `fields = "*"`)
   - Third-party plugins: Must declare dependencies
   - Untrusted plugins: Not supported in MVP

---

## Action Items

- [ ] Decide between Option A, B, or C before Phase 2 Plugin SDK design
- [ ] If Option B: Add field filtering to serialization before tap invocation
- [ ] Document sensitive field handling (passwords, tokens never serialized)
- [ ] Add output validation to Kernel render pipeline

---

## Appendix: Why Handle-Based Doesn't Solve the Problem

Even with handle-based access, a determined malicious plugin could:

1. Iterate all known field names: `for field in KNOWN_FIELDS { get_field(handle, field) }`
2. Brute-force field discovery: Try common field names
3. Timing attacks: Measure response time differences

Handle-based access provides **defense in depth**, not **absolute security**. The real protection comes from:
- Plugin sandboxing (no exfiltration channels)
- Output validation (Kernel strips unauthorized data)
- Permission system (restricted host functions)

Full-serialization shifts work from "block reads" to "validate outputs" - both are viable security models.
