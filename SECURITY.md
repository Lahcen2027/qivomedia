# Security

## Reporting a vulnerability

Please email **security@qivomedia.com** (or the contact in `/.well-known/security.txt` on your install) with steps to reproduce. We acknowledge reports within 2 business days and ask that you give us a reasonable time to fix the issue before disclosing it. Don't run automated scanners against other customers' installations.

## How Qivomedia protects you

Controls are grouped by the [OWASP ASVS](https://owasp.org/www-project-application-security-verification-standard/) chapter they address.

### Authentication (V2, V6)
- **Passwords** follow NIST SP 800-63B. They must be 12 to 128 characters long and are checked against a list of common passwords, against your own name, email and organization, and against [Have I Been Pwned](https://haveibeenpwned.com/Passwords) using k-anonymity: only the first 5 characters of a SHA-1 hash leave the server. If HIBP is unreachable, the check is skipped rather than blocking you. Set `QIVO_HIBP=0` to turn it off for air-gapped installs.
- **Password storage** uses Argon2id (19 MiB memory, 2 iterations, the OWASP minimum) with a unique salt per password.
- **Two-factor authentication**:
  - Codes come from an authenticator app (TOTP, RFC 6238), with 10 single-use recovery codes stored only as hashes.
  - A code is accepted for its 30-second window plus one window either side, and a used code can't be reused.
  - The sign-in step between password and code lasts 5 minutes and allows 5 attempts.
  - Organizations can **require 2FA for every member**. This is enforced on the server, so members who haven't set it up can only reach the setup screen.
  - A password-reset link never signs in an account that has 2FA. The person still has to enter their password and a code.
- **Brute force:** an account locks for 15 minutes after 5 failed sign-ins, and an IP address after 30. Sign-up, password reset, token checks, 2FA steps, screen registration and webhooks are all rate-limited.
- **Account recovery:** reset links work once, expire after 1 hour (invitations after 7 days), and are stored only as hashes. The reply is the same whether or not an account exists.
- **Lost 2FA device:** there are four ways back in, from most to least common:
  1. a recovery code, used in place of the 6-digit code;
  2. an admin of the same organization clicks **Reset 2FA** in Team;
  3. the platform operator resets it from **Customers & usage → Users**. The operator must have 2FA on, can't reset their own, and must write down how they verified the person's identity. That reason goes into both the customer's and the operator's audit log;
  4. someone with access to the server runs `qivo-server reset-mfa <email>`. This is the last resort, and the only way to recover the platform admin.

  Every reset signs the person out everywhere, invalidates their old authenticator and recovery codes, and emails them, so a reset they didn't ask for doesn't go unnoticed.
- **Single sign-on** (OpenID Connect: Microsoft Entra ID, Google Workspace, Keycloak and others; set-up in `SSO.md`):
  - Authorization code flow with **PKCE (S256)**, a single-use `state` that expires after 10 minutes and is tied to the browser by a cookie, and a `nonce` checked inside the ID token.
  - ID tokens are verified against the provider's published keys (JWKS, refreshed when keys rotate), plus issuer, audience and expiry. Providers must use HTTPS.
  - Identities are linked by the provider's stable subject id. Microsoft accounts are matched by **UPN** (whose domains the tenant must verify) instead of the editable `email` claim, and guest accounts are refused. Google accounts must belong to the listed Workspace domain (`hd`), with a verified email.
  - An SSO sign-in never takes over an account in another organization.
  - Client secrets are encrypted at rest with **AES-256-GCM**. The key is `QIVO_SECRET_KEY` or `secret.key` (owner-only) in the data folder.
  - Organizations can **require SSO**. Members then can't use a password, while owners and admins keep theirs as an emergency way in.
  - Multi-factor sign-in is the identity provider's job for SSO sessions, so the organization's 2FA policy doesn't ask for a second code.

### Sessions (V3)
- The session token sits only in an **HttpOnly cookie**, so JavaScript can't read it and a script-injection (XSS) bug can't steal it. Over HTTPS the cookie is named `__Host-qivo_session` (Secure, host-bound, `Path=/`), and it's always `SameSite=Strict`.
- Tokens never appear in URLs, logs or browser storage, and the server stores only a SHA-256 hash of each.
- Sessions end after **30 days**, or after **12 hours without use** (set with `QIVO_SESSION_IDLE_HOURS`).
- Everyone can see their active sessions (device, IP address, last active) and sign any of them out. Changing a password signs out every other session, and a reset or turning on 2FA signs out all sessions without the second factor.

### Access control (V4)
- Every request is scoped to the caller's organization, including IDs referenced inside saved content.
- Roles (viewer, editor, admin, owner) are checked on the server for each action. Platform-admin rights are separate from organization roles.

### Input, files and browser protections (V5, V12, V14)
- **Uploads are identified by their content**, not their file name or the type the browser claims. Executables, archives and unknown binary files are refused, as are files whose content doesn't match their declared type.
- **Uploaded HTML and SVG files are served sandboxed** (`Content-Security-Policy: sandbox`). They run with an opaque origin and can never act as the dashboard.
- Each area has its own **Content Security Policy**:
  - **Dashboard:** no inline or third-party scripts at all.
  - **Player:** HTML widgets run inside sandboxed iframes.
  - **API:** blocks all content.
- **Other headers:** `Permissions-Policy` (camera, microphone, geolocation and payment off), `Cross-Origin-Opener-Policy`, `Cross-Origin-Resource-Policy`, `X-Frame-Options`/`frame-ancestors`, `nosniff`, `Referrer-Policy`, HSTS, and `Cache-Control: no-store` on every API response.
- **CSRF:** state-changing requests made with the cookie must come from the app's own origin, checked through `Origin` and Fetch Metadata. This sits on top of `SameSite=Strict`.
- Cross-origin requests (CORS) aren't enabled. The website reaches the app through the same origin.

### Logging (V7)
The audit log records sign-ins (with IP address and browser), failed sign-ins, 2FA being turned on or off, recovery-code use, 2FA resets (by an admin, the platform operator or a server command, with who did it and why), session sign-outs, password changes, role changes and alerts. Admins see it under **Team & audit**.

### Infrastructure and supply chain (V10, V14)
- **Containers:**
  - The app runs as a non-root user on a **read-only filesystem**, where only `/data` and `/tmp` are writable.
  - It has **no Linux capabilities**, `no-new-privileges` is set, and the number of processes is capped. Caddy gets only `NET_BIND_SERVICE`.
- **HTTPS** is automatic through Caddy and Let's Encrypt. The app trusts only the client address appended by its own proxy.
- `deploy/.env` holds secrets, and the installer sets it to `chmod 600`.
- **On every push and every week, CI runs:**
  - the test suite, including attack-style tests: CSRF, session theft, 2FA replay, disguised executables, malicious SVG;
  - **cargo-audit** against the RustSec advisory database;
  - a **Trivy** scan of the Docker image (it fails on fixable critical or high issues);
  - and it produces an **SBOM** (SPDX).
- **Dependabot** proposes updates weekly for Rust crates, GitHub Actions and the Docker base images.
- Exceptions to the dependency audit live in `.cargo/audit.toml`, each with its reason.

## Known limitations / roadmap
- **Passkeys (WebAuthn)** for passwordless and phishing-resistant sign-in are next.
- **SAML 2.0** single sign-on (for universities without OpenID Connect) is planned. OpenID Connect is built in.
- **Screen device keys** live in the screen's browser storage. A key only lets that one screen read its own content, and an admin can revoke it by deleting the screen.
- **Rate limits** are kept per server process. Running several app instances will need a shared store, such as Redis or Postgres.
- 2FA secrets are stored in plain text in the database, which is protected by the server's own access controls and the encrypted backups you make. Encrypting them with the same key as the SSO secrets is planned.
