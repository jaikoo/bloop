# Passkey / WebAuthn Setup Guide

bloop uses passkeys (WebAuthn/FIDO2) for dashboard authentication. This provides phishing-resistant auth with no passwords to manage.

## Prerequisites

- A browser that supports WebAuthn (Chrome, Firefox, Safari, Edge)
- A FIDO2 authenticator: built-in biometrics (Touch ID, Windows Hello), a hardware security key (YubiKey), or a phone-based passkey

## Configuration

Before first use, set the relying party (RP) fields in `config.toml` or via environment variables:

```toml
[auth]
hmac_secret = "your-ingest-hmac-secret"
rp_id = "bloop.example.com"        # Your domain (no port, no scheme)
rp_origin = "https://bloop.example.com"  # Full origin URL
session_ttl_secs = 604800            # 7 days (default)
```

Or via environment:

```bash
BLOOP__AUTH__RP_ID=bloop.example.com
BLOOP__AUTH__RP_ORIGIN=https://bloop.example.com
BLOOP__AUTH__SESSION_TTL_SECS=604800
```

**Important:** `rp_id` must match the domain users access the dashboard from. For local development, use `localhost`. For production, use your actual domain.

## Initial Setup (First User Registration)

1. Start bloop and navigate to the dashboard URL (e.g., `https://bloop.example.com/`)
2. You'll be redirected to the login page, which detects no users exist and shows the **Initial Setup** screen
3. Enter an admin username (this is a display name, not used for login)
4. Click **Register Passkey**
5. Your browser will prompt you to use your authenticator:
   - **Touch ID / Windows Hello**: Use your biometric
   - **Security key**: Touch or insert your key
   - **Phone passkey**: Scan the QR code with your phone
6. On success, you're logged in and redirected to the dashboard

> Registration is one-time only. Once a user exists, the registration endpoint is permanently locked.

## Logging In

1. Navigate to the dashboard
2. If your session has expired, you'll see the **Sign In** page
3. Click **Sign In with Passkey**
4. Authenticate with your passkey (biometric, security key, or phone)
5. You're redirected to the dashboard

## Logging Out

Click the **Logout** button in the top-right of the dashboard header. This destroys your server-side session and clears the cookie.

## How It Works

- **Registration** creates a FIDO2 credential bound to your domain. The public key is stored in the bloop database; the private key never leaves your authenticator.
- **Login** uses a challenge-response protocol. The server sends a random challenge, your authenticator signs it, and the server verifies the signature against the stored public key.
- **Sessions** are random 32-byte tokens stored in an HttpOnly cookie (`bloop_session`). They're validated against the database on every request and expire after `session_ttl_secs`.

## What's Protected

| Route | Auth |
|-------|------|
| `/` (dashboard) | Session (redirect to login) |
| `/v1/errors/*`, `/v1/stats`, `/v1/trends` | Bearer token or Session |
| `/v1/alerts/*` | Bearer token or Session |
| `/v1/releases/*` | Bearer token or Session |
| `/v1/projects/*` | Session |
| `/v1/tokens/*` | Session |
| `/v1/analytics/*` | Bearer token or Session |
| `/v1/admin/users/*/role` | Admin Session |
| `/v1/admin/retention`, `/v1/admin/retention/purge` | Admin Session |
| `/v1/ingest/*` | HMAC signature (unchanged) |
| `/health` | Public |
| `/auth/*` | Public |

## Troubleshooting

### "Registration is closed"
A user already exists. Only one user can register. If you need to reset, delete the `webauthn_users`, `webauthn_credentials`, and `sessions` tables from the SQLite database.

### Passkey prompt doesn't appear
- Ensure your browser supports WebAuthn
- For HTTPS origins, ensure your TLS certificate is valid
- For `localhost`, most browsers allow WebAuthn over HTTP

### "Challenge expired or invalid"
The registration/login challenge has a 120-second TTL. Restart the flow.

### Session expired
Sessions last 7 days by default. Log in again, or increase `session_ttl_secs`.

### Wrong domain error
The `rp_id` in your config must exactly match the domain in your browser's address bar. If you access via `bloop.example.com`, set `rp_id = "bloop.example.com"`.
