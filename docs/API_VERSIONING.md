# API Versioning and Schema Evolution

**Issue #1192: Add API Contract Testing Against Schema**

## Overview

This document defines the API versioning strategy and schema evolution process for QuorumCredit, ensuring backward compatibility and preventing breaking changes from reaching production.

## Semantic Versioning

The QuorumCredit API follows [Semantic Versioning 2.0.0](https://semver.org/):

- **MAJOR** version (e.g., 1.0.0 → 2.0.0): Breaking changes that require client updates
- **MINOR** version (e.g., 1.0.0 → 1.1.0): New features, backward compatible
- **PATCH** version (e.g., 1.0.0 → 1.0.1): Bug fixes, backward compatible

Current API version: **1.0.0**

## Schema Contract

All API responses conform to the OpenAPI 3.0.0 specification defined in `openapi.yaml`.

### Response Structure

Every API response includes a schema version identifier:

```json
{
  "api_version": "1.0.0",
  "contract_version": 1,
  "data": { /* response data */ }
}
```

### Required Fields

The following fields are **required** in all responses and must never be removed or renamed:

- **Success Responses (2xx)**:
  - `api_version`: API semantic version
  - `contract_version`: On-chain contract version
  - `data`: Response payload

- **Error Responses (4xx, 5xx)**:
  - `api_version`: API semantic version
  - `error_code`: Standardized error code
  - `error_message`: Human-readable error description
  - `trace_id`: Request tracking ID for debugging

## Breaking vs Non-Breaking Changes

### Non-Breaking Changes (PATCH/MINOR)

The following changes are safe and **never** bump MAJOR version:

✅ Adding new **optional** fields to response objects
✅ Deprecating fields (with grace period)
✅ Adding new endpoints
✅ Making required request fields optional
✅ Narrowing error conditions
✅ Improving error messages

### Breaking Changes (MAJOR)

The following changes **always** bump MAJOR version:

❌ Removing required fields from responses
❌ Changing field types (e.g., `string` → `integer`)
❌ Renaming existing fields
❌ Removing endpoints
❌ Making optional request fields required
❌ Changing error codes
❌ Restructuring response objects

## Deprecation Process

When a field or endpoint must be removed:

1. **Announce** (v1.0.0): Field marked as deprecated in schema with `x-deprecated: true`
2. **Warn** (v1.1.0+): Deprecation warning returned in response headers
3. **Remove** (v2.0.0): Field removed in major version bump

### Deprecation Header

```
Deprecation: true
Sunset: Wed, 21 Dec 2025 23:59:59 GMT
Link: <https://docs.quorumcredit.io/migration>; rel="deprecation"
```

## Schema Regression Testing

All API responses are validated at test time against the OpenAPI schema to prevent breaking changes:

```bash
# Run schema contract tests
cargo test api_contract_test

# Validate OpenAPI spec
openapi-validator openapi.yaml

# Check for breaking changes
schema-diff v1.0.0 v1.1.0
```

## Field Naming Conventions

All field names follow `snake_case`:

```json
{
  "loan_id": "123",
  "borrower_address": "GXX...",
  "created_timestamp": 1234567890,
  "maturity_timestamp": 1234567890
}
```

## Numeric Field Precision

All monetary amounts are represented as **strings** in the API to preserve precision:

```json
{
  "amount": "1000000000",  // 100 XLM in stroops
  "yield_earned": "2000000",
  "slash_amount": "500000000"
}
```

This prevents floating-point precision loss when handling amounts up to `i128`.

## Timestamp Format

All timestamps use **Unix timestamp format** (seconds since epoch):

```json
{
  "created_timestamp": 1690000000,
  "maturity_timestamp": 1690604800
}
```

Precision: Seconds (no fractional seconds)

## Array Responses and Pagination

Paginated list endpoints return responses with pagination metadata:

```json
{
  "api_version": "1.0.0",
  "contract_version": 1,
  "data": {
    "items": [ /* array items */ ],
    "pagination": {
      "page": 1,
      "limit": 50,
      "total": 150,
      "has_more": true
    }
  }
}
```

- `page`: Current page (1-indexed)
- `limit`: Items per page
- `total`: Total number of items
- `has_more`: Whether more pages exist

## Error Response Format

All error responses follow a standardized format:

```json
{
  "api_version": "1.0.0",
  "error_code": "INSUFFICIENT_VOUCHES",
  "error_message": "Borrower does not have sufficient vouches for the requested amount",
  "trace_id": "req_abc123def456",
  "details": {
    "required_stake": "1000000000",
    "available_stake": "500000000",
    "deficit": "500000000"
  }
}
```

### Error Codes

Error codes are uppercase `SCREAMING_SNAKE_CASE`:

- `INVALID_REQUEST` (400): Malformed request parameters
- `UNAUTHORIZED` (401): Missing or invalid credentials
- `INSUFFICIENT_VOUCHES` (400): Insufficient collateral
- `LOAN_NOT_FOUND` (404): Loan does not exist
- `INVALID_STATE_TRANSITION` (400): Illegal state change
- `INTERNAL_ERROR` (500): Unexpected server error

## Monitoring and Alerts

### Schema Violation Detection

Responses are validated in real-time against the OpenAPI schema:

- **Alert**: Schema violation detected
- **Action**: Immediate incident response
- **Escalation**: Engineering team + on-call

### Breaking Change Detection

CI/CD pipeline checks for breaking changes:

```yaml
# .github/workflows/schema-check.yml
- name: Check for breaking schema changes
  run: |
    openapi-generator validate -i openapi.yaml
    schema-diff $PREVIOUS_VERSION openapi.yaml --fail-on-breaking
```

## Client Compatibility

### Version Discovery

Clients can determine supported API versions:

```
GET /api-versions
```

Response:
```json
{
  "supported_versions": ["1.0.0"],
  "latest_version": "1.0.0",
  "deprecated_versions": []
}
```

### Client Requirements

Clients **must**:

1. Check `api_version` in response to ensure compatibility
2. Ignore unknown optional fields
3. Handle deprecation warnings gracefully
4. Plan migration before sunset date

## Contract Version vs API Version

- **Contract Version**: Semantic version of on-chain smart contract (e.g., 1.0.0)
- **API Version**: Semantic version of REST/external API (e.g., 1.0.0)

These may diverge as the on-chain contract evolves independently of the API layer.

## Migration Guide Template

When releasing a major version bump:

```markdown
# Migration Guide: API v1.0.0 → v2.0.0

## Breaking Changes

### Removed Fields
- `field_name`: Use `new_field_name` instead

### Renamed Endpoints
- `/old-endpoint` → `/new-endpoint`

### Changed Response Format
- Response structure: `{ old_format }` → `{ new_format }`

## Migration Steps

1. Update all clients to use new field names
2. Switch to new endpoint URLs
3. Test in staging environment
4. Deploy to production

## Rollback Plan

If issues arise, maintain v1.0.0 parallel to v2.0.0 for 90 days.
```

## References

- [OpenAPI 3.0.0 Specification](https://spec.openapis.org/oas/v3.0.0)
- [Semantic Versioning 2.0.0](https://semver.org/)
- [API Deprecation Best Practices](https://tools.ietf.org/html/draft-wilde-http-sunset-header)
- [JSON Schema Validation](https://json-schema.org/)
