# Git AI Standard v3.1.0

This document defines the incremental schema change from [Git AI Standard v3.0.0](./git_ai_standard_v3.0.0.md).

Unless explicitly overridden below, all requirements from v3.0.0 remain in force.

## 1. Schema Version

The schema version for this revision is:

```
authorship/3.1.0
```

Implementations that write the `x_user_id` metadata field defined in this revision MUST emit `authorship/3.1.0` in the `schema_version` field.

Readers SHOULD accept both `authorship/3.0.0` and `authorship/3.1.0` for backward compatibility.

## 2. Metadata Section

The metadata section continues to be a valid JSON object as defined in v3.0.0, with the following additional optional field.

### 2.1 Optional Fields

| Field | Type | Description |
|-------|------|-------------|
| `git_ai_version` | string | Version of the git-ai tool that generated this log |
| `x_user_id` | string | Remote user identifier resolved at commit time, typically sourced from MCP configuration or an explicit environment override |

### 2.2 `x_user_id` Semantics

- `x_user_id` is OPTIONAL.
- When present, it MUST be serialized in the metadata JSON section.
- When present, it SHOULD contain the non-empty remote user identifier that was available to the committing environment at note generation time.
- Writers MUST omit the field when no user identifier can be resolved.
- Consumers MUST tolerate the field being absent.

### 2.3 Example

```
---
{
  "schema_version": "authorship/3.1.0",
  "git_ai_version": "1.3.3",
  "x_user_id": "20260422",
  "base_commit_sha": "7734793b756b3921c88db5375a8c156e9532447b",
  "prompts": {}
}
```