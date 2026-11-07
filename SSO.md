# Single sign-on (university account)

Staff sign in to Qivomedia with their university account through **Microsoft 365 (Entra ID)**, **Google Workspace** or any **OpenID Connect** provider (Keycloak, Okta, ADFS 2016+, Shibboleth with OIDC). The university's IT department keeps control of who has access, and of multi-factor sign-in.

**Where:** dashboard → **Team & audit** → **Single sign-on** (organization admins).

**Before you start:** the server must know its public address. Set `QIVO_PUBLIC_URL=https://signage.your-university.ma` in `.env` (see `DEPLOY.md`). The **Redirect URI** shown on that page is what IT registers.

## Microsoft 365 / Entra ID

Done by a university IT administrator, in about 10 minutes:

1. Go to **entra.microsoft.com** → *Identity* → *Applications* → **App registrations** → **New registration**.
   - Name: `Qivomedia`.
   - Supported account types: **Accounts in this organizational directory only** (single tenant).
   - Redirect URI: platform **Web**, then paste the Redirect URI from Qivomedia (`https://…/api/auth/sso/callback`).
2. On the app's **Overview** page, copy:
   - **Application (client) ID**
   - **Directory (tenant) ID**
3. **Certificates & secrets** → **New client secret**. Copy the **Value** (shown only once). Note its expiry date: a new secret has to be entered in Qivomedia before it expires.
4. Optional, to give roles from Entra groups: **Token configuration** → **Add groups claim** → *Security groups* (ID token: *Group ID*). Then copy each group's **Object ID** from *Groups*.
5. In Qivomedia: **+ Microsoft 365**, then paste:
   - the tenant ID, client ID and secret;
   - the allowed email domains (for example `univ.ma`);
   - optionally, the group rules, one per line: `<object id> = editor`.

**Notes**
- Qivomedia identifies people by their **sign-in name (UPN)**, not the editable "email" attribute. UPN domains must be verified in your tenant, which prevents account takeover through a changed email attribute. Guest (B2B) accounts are refused.
- Multi-factor authentication and Conditional Access policies apply as usual: Qivomedia doesn't ask for its own 2FA code after an SSO sign-in.
- Someone in more than 200 groups doesn't receive the group list in the token, so they get the default role. In that case, use fewer, dedicated groups (for example `Qivomedia Editors`).

## Google Workspace

1. **console.cloud.google.com** → choose or create a project → **APIs & Services** → **OAuth consent screen** → User type **Internal**, app name `Qivomedia`.
2. **Credentials** → **Create credentials** → **OAuth client ID** → *Web application*. Authorized redirect URI: the Redirect URI from Qivomedia.
3. Copy the **Client ID** and **Client secret**.
4. In Qivomedia: **+ Google Workspace**. Paste them and enter your Workspace domain (required, for example `univ.ma`).

**Notes**
- Only accounts of the listed Workspace domains can sign in. Personal Gmail accounts are refused even if someone tries.
- Google doesn't put group memberships in the sign-in token, so roles are set in Qivomedia (default role, then change roles in Team).

## Other providers (OpenID Connect)

Register a **confidential client** using the authorization code flow, with the Redirect URI above and the scopes `openid email profile`. In Qivomedia: **+ Other provider**, then enter:
- the **Issuer URL** (the address that serves `/.well-known/openid-configuration`, for example `https://sso.univ.ma/realms/staff`);
- the client ID and secret;
- the allowed domains.

Group rules use the names your provider sends in the `groups` claim. The provider must use HTTPS.

## Options

| Option | |
|---|---|
| **Allowed email domains** | Who may sign in. The login page also uses it to offer the right button when someone types their email. A domain can belong to only one provider on a server. |
| **Create an account the first time someone signs in** | On: anyone allowed by the provider gets an account. Off: only people already invited in Team (matched by email). |
| **Role for new accounts** | Usually **viewer**. With group rules, people in no listed group get this role. |
| **Roles from groups** | Applied at every sign-in, and the highest matching role wins. Leave empty to manage roles only in Qivomedia. The owner's role is never changed. |
| **Require single sign-on for members** | Members can no longer use a password. **Owners and admins keep their password** as an emergency way in, in case the provider or its secret breaks. |

## Troubleshooting

The login page explains why a sign-in failed. The most common reasons:

| Message | Fix |
|---|---|
| "…isn't from an email domain your organization allows" | Add the domain under *Allowed email domains*, or the person used a personal or other account. |
| "You don't have a Qivomedia account yet" | Account creation is off: invite the person in Team with the same email. |
| "…couldn't be verified" | Usually a wrong or expired **client secret**, or the redirect URI registered with the provider doesn't exactly match. Check `qivo logs qivomedia`. |
| "…couldn't be reached" | The server can't reach the provider (firewall or proxy). |
| "…already belongs to another Qivomedia organization" | The email is used in another organization on this server: contact the platform operator. |

**Security details**
- Authorization code flow with PKCE, a single-use `state` tied to the browser, and a `nonce`.
- ID tokens are verified against the provider's published keys (issuer, audience, expiry, nonce).
- Client secrets are stored encrypted with AES-256-GCM (`secret.key` in the data folder, or `QIVO_SECRET_KEY`).
- Every SSO sign-in, account creation and role change is in the audit log.
