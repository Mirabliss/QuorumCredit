# Webhook Signature Verification Guide

> **#1082**: Add Webhook Signature Verification

## Overview

QuorumCredit now supports HMAC-SHA256 signature verification for webhook requests to prevent spoofing attacks. This guide covers how to register webhooks, verify signatures, and implement webhook receivers securely.

## Why Webhook Signature Verification?

Without signature verification, malicious actors could:
- Spoof webhook events from QuorumCredit
- Trigger false loan or vouch notifications
- Manipulate external systems that depend on webhook events

HMAC-SHA256 signatures ensure that:
1. **Authenticity**: Webhooks genuinely originate from QuorumCredit
2. **Integrity**: Payload hasn't been tampered with during transmission
3. **Freshness**: Webhooks aren't replay attacks (5-minute window)

## Quick Start

### 1. Register a Webhook

```bash
curl -X POST http://localhost:3000/api/webhooks/register \
  -H "Content-Type: application/json" \
  -d '{
    "url": "https://your-service.com/webhooks/quorumcredit",
    "events": ["loan.requested", "loan.repaid", "vouch.created"]
  }'
```

Response:
```json
{
  "id": "wh_1742908800000_abc123",
  "url": "https://your-service.com/webhooks/quorumcredit",
  "createdAt": "2026-07-25T10:11:54.726Z",
  "lastUsed": null,
  "events": ["loan.requested", "loan.repaid", "vouch.created"],
  "enabled": true,
  "message": "Webhook registered successfully. Save the secret shown below - it will not be shown again.",
  "secret": "a1b2c3d4e5f6... (64-character hex string)"
}
```

**⚠️ Important**: Save the `secret` immediately! It's only shown once during registration.

### 2. Receive and Verify Webhooks

When QuorumCredit sends a webhook to your endpoint, it includes these headers:

```
X-Webhook-Event: loan.requested
X-Webhook-Timestamp: 1742908800000
X-Webhook-Signature: a1b2c3d4e5f6...
X-Webhook-Signature-Version: hmac-sha256
X-Webhook-Id: wh_1742908800000_abc123
Content-Type: application/json
```

The request body contains the event payload.

### 3. Verify the Signature

Example verification in Node.js:

```javascript
const crypto = require('crypto');

function verifyWebhookSignature(payload, signature, secret) {
  const hmac = crypto.createHmac('sha256', secret);
  hmac.update(JSON.stringify(payload));
  const expectedSignature = hmac.digest('hex');
  
  // Use timingSafeEqual to prevent timing attacks
  return crypto.timingSafeEqual(
    Buffer.from(signature, 'hex'),
    Buffer.from(expectedSignature, 'hex')
  );
}

// Example usage
const isValid = verifyWebhookSignature(
  requestBody,
  requestHeaders['x-webhook-signature'],
  YOUR_WEBHOOK_SECRET
);
```

## API Reference

### Webhook Registration

**POST /api/webhooks/register**

Register a new webhook endpoint.

**Request Body:**
```json
{
  "url": "string (required)",
  "events": ["string array (required)"]
}
```

**Valid Events:**
- `loan.requested` - Loan application submitted
- `loan.disbursed` - Loan funds transferred to borrower
- `loan.repaid` - Loan fully repaid
- `loan.defaulted` - Loan marked as default
- `vouch.created` - New vouch created
- `vouch.withdrawn` - Vouch withdrawn
- `slash.executed` - Slash executed on default
- `config.updated` - Protocol configuration updated

### List Webhooks

**GET /api/webhooks**

List all registered webhooks (secrets are not included).

### Get Webhook

**GET /api/webhooks/{id}**

Get details for a specific webhook.

### Update Webhook

**PUT /api/webhooks/{id}**

Update webhook configuration.

**Request Body:**
```json
{
  "url": "string (optional)",
  "events": ["string array (optional)"],
  "enabled": "boolean (optional)"
}
```

### Delete Webhook

**DELETE /api/webhooks/{id}**

Delete a webhook registration.

### Test Webhook

**POST /api/webhooks/{id}/test**

Send a test webhook to verify your endpoint works.

**Request Body:**
```json
{
  "event": "string (required)",
  "data": "any (required)"
}
```

## Webhook Payload Structure

All webhook payloads follow this structure:

```json
{
  "event": "loan.requested",
  "data": {
    // Event-specific data
    "borrower": "GABCD...",
    "amount": 5000000000,
    "threshold": 10000000000,
    "loanPurpose": "Business expansion",
    "token": "CDLM...",
    "timestamp": 1742908800000
  },
  "timestamp": 1742908800000,
  "webhookId": "wh_1742908800000_abc123"
}
```

### Event-specific Data

#### loan.requested
```json
{
  "borrower": "Stellar address",
  "amount": "loan amount in stroops",
  "threshold": "required stake in stroops",
  "loanPurpose": "description",
  "token": "token contract address",
  "timestamp": "Unix timestamp in milliseconds"
}
```

#### loan.repaid
```json
{
  "borrower": "Stellar address",
  "payment": "repayment amount in stroops",
  "principal": "principal portion",
  "yield": "yield portion",
  "timestamp": "Unix timestamp in milliseconds"
}
```

#### vouch.created
```json
{
  "voucher": "Stellar address",
  "borrower": "Stellar address",
  "stake": "stake amount in stroops",
  "token": "token contract address",
  "timestamp": "Unix timestamp in milliseconds"
}
```

## Security Best Practices

### 1. Store Secrets Securely
- Never commit webhook secrets to version control
- Use environment variables or secret management systems
- Rotate secrets periodically (minimum: every 90 days)

### 2. Verify Signatures Immediately
```javascript
// Good: Verify before processing
function handleWebhook(request) {
  if (!verifySignature(request)) {
    return { error: 'Invalid signature' };
  }
  processEvent(request.body);
}

// Bad: Processing before verification
function handleWebhook(request) {
  processEvent(request.body); // Vulnerable to spoofing!
  verifySignature(request);   // Too late
}
```

### 3. Check Timestamp Freshness
```javascript
function isFreshTimestamp(timestamp) {
  const now = Date.now();
  const fiveMinutes = 5 * 60 * 1000;
  return Math.abs(now - timestamp) <= fiveMinutes;
}
```

### 4. Implement Retry Logic with Idempotency
```javascript
// Use webhookId to prevent duplicate processing
const processedWebhooks = new Set();

function handleWebhook(webhook) {
  if (processedWebhooks.has(webhook.webhookId)) {
    return; // Already processed
  }
  
  // Process webhook...
  processedWebhooks.add(webhook.webhookId);
}
```

### 5. Log Security Events
```javascript
function handleWebhook(request) {
  if (!verifySignature(request)) {
    console.warn('Invalid webhook signature', {
      webhookId: request.headers['x-webhook-id'],
      ip: request.ip,
      timestamp: new Date().toISOString()
    });
    return { error: 'Invalid signature' };
  }
  
  console.info('Valid webhook received', {
    event: request.body.event,
    webhookId: request.headers['x-webhook-id'],
    timestamp: new Date().toISOString()
  });
}
```

## Implementation Examples

### Node.js with Express
```javascript
const express = require('express');
const crypto = require('crypto');
const app = express();

const WEBHOOK_SECRET = process.env.WEBHOOK_SECRET;

app.post('/webhooks/quorumcredit', express.json(), (req, res) => {
  // Extract headers
  const signature = req.headers['x-webhook-signature'];
  const timestamp = req.headers['x-webhook-timestamp'];
  const event = req.headers['x-webhook-event'];
  const webhookId = req.headers['x-webhook-id'];
  
  // Verify timestamp freshness
  if (!isFreshTimestamp(parseInt(timestamp))) {
    return res.status(400).json({ error: 'Timestamp too old' });
  }
  
  // Verify signature
  const payload = {
    event,
    data: req.body,
    timestamp: parseInt(timestamp),
    webhookId
  };
  
  if (!verifyWebhookSignature(payload, signature, WEBHOOK_SECRET)) {
    return res.status(401).json({ error: 'Invalid signature' });
  }
  
  // Process event
  switch (event) {
    case 'loan.requested':
      handleLoanRequest(payload.data);
      break;
    case 'loan.repaid':
      handleLoanRepayment(payload.data);
      break;
    // ... other events
  }
  
  res.json({ status: 'accepted' });
});

function verifyWebhookSignature(payload, signature, secret) {
  const hmac = crypto.createHmac('sha256', secret);
  hmac.update(JSON.stringify(payload));
  const expectedSignature = hmac.digest('hex');
  return crypto.timingSafeEqual(
    Buffer.from(signature, 'hex'),
    Buffer.from(expectedSignature, 'hex')
  );
}

function isFreshTimestamp(timestamp) {
  const now = Date.now();
  return Math.abs(now - timestamp) <= 5 * 60 * 1000; // 5 minutes
}
```

### Python with Flask
```python
from flask import Flask, request, jsonify
import hmac
import hashlib
import json
import time

app = Flask(__name__)
WEBHOOK_SECRET = os.environ.get('WEBHOOK_SECRET')

def verify_signature(payload, signature):
    expected_signature = hmac.new(
        WEBHOOK_SECRET.encode('utf-8'),
        json.dumps(payload, sort_keys=True).encode('utf-8'),
        hashlib.sha256
    ).hexdigest()
    return hmac.compare_digest(expected_signature, signature)

@app.route('/webhooks/quorumcredit', methods=['POST'])
def handle_webhook():
    signature = request.headers.get('X-Webhook-Signature')
    timestamp = request.headers.get('X-Webhook-Timestamp')
    event = request.headers.get('X-Webhook-Event')
    webhook_id = request.headers.get('X-Webhook-Id')
    
    # Check timestamp freshness
    current_time = int(time.time() * 1000)
    if abs(current_time - int(timestamp)) > 300000:  # 5 minutes
        return jsonify({'error': 'Timestamp too old'}), 400
    
    # Verify signature
    payload = {
        'event': event,
        'data': request.json,
        'timestamp': int(timestamp),
        'webhookId': webhook_id
    }
    
    if not verify_signature(payload, signature):
        return jsonify({'error': 'Invalid signature'}), 401
    
    # Process event
    if event == 'loan.requested':
        handle_loan_request(payload['data'])
    elif event == 'loan.repaid':
        handle_loan_repayment(payload['data'])
    
    return jsonify({'status': 'accepted'})
```

## Troubleshooting

### Common Issues

#### 1. "Invalid signature" error
- Check that you're using the correct secret
- Verify payload serialization (must be exact JSON string)
- Ensure headers are correctly cased (`X-Webhook-Signature`, not `x-webhook-signature`)

#### 2. "Timestamp too old" error
- Check server time synchronization
- Ensure webhooks are processed within 5 minutes
- Verify timestamp is in milliseconds, not seconds

#### 3. Webhook not being sent
- Verify webhook is enabled (`enabled: true`)
- Check that the event type is in your subscribed events
- Monitor server logs for delivery errors

#### 4. Duplicate webhooks
- Use `webhookId` for idempotency
- Implement deduplication in your receiver
- Check if retries are causing duplicates

### Debugging Steps

1. **Enable detailed logging**:
```javascript
console.log('Webhook received:', {
  headers: request.headers,
  body: request.body,
  timestamp: new Date().toISOString()
});
```

2. **Test with the test endpoint**:
```bash
curl -X POST http://localhost:3000/api/webhooks/{id}/test \
  -H "Content-Type: application/json" \
  -d '{
    "event": "loan.requested",
    "data": {"test": true}
  }'
```

3. **Verify signature manually**:
```javascript
// Log what's being signed
console.log('Payload being signed:', JSON.stringify(payload));
console.log('Received signature:', signature);
console.log('Expected signature:', expectedSignature);
```

## Monitoring and Alerting

### Recommended Metrics
- `webhook_received_total` - Total webhooks received
- `webhook_verified_total` - Successfully verified webhooks
- `webhook_rejected_total` - Rejected webhooks (with reason)
- `webhook_processing_duration_seconds` - Time to process webhooks

### Alerting Rules
- Alert if more than 5% of webhooks fail verification
- Alert if webhook processing latency exceeds 10 seconds
- Alert if no webhooks received for 1 hour (during business hours)

## Compliance and Standards

### HMAC-SHA256 Implementation
- Uses constant-time comparison (`timingSafeEqual`) to prevent timing attacks
- 256-bit keys (64 hex characters) for sufficient entropy
- SHA-256 hash function (NIST-approved)

### Data Privacy
- Webhook payloads may contain sensitive financial data
- Implement end-to-end encryption if required by regulations
- Log minimal personally identifiable information (PII)

### Regulatory Considerations
- **GDPR**: Ensure lawful basis for processing, implement data minimization
- **PCI DSS**: If handling payment information, ensure proper security controls
- **Financial regulations**: Maintain audit trails of all financial events

## Migration from Unsigned Webhooks

If you were previously using unsigned webhooks:

1. **Register new signed webhooks** using the API
2. **Update your receiver** to verify signatures
3. **Run both systems in parallel** during migration
4. **Monitor** for any issues with the new system
5. **Disable old webhooks** once migration is complete

## Support

For issues with webhook signature verification:
1. Check this documentation
2. Review server logs for error messages
3. Contact support with:
   - Webhook ID
   - Timestamp of the issue
   - Error messages received
   - Your verification code snippet (sanitized of secrets)

## Changelog

### v1.0.0 (2026-07-25)
- Initial implementation of HMAC-SHA256 webhook signature verification
- Webhook registration API
- Comprehensive verification guide
- Security best practices documentation