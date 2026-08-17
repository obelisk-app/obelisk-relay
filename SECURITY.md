# Security policy

**Contact: security@obelisk.ar**

## Please do not open a public issue

Report privately to **security@obelisk.ar**.

## Scope

**In scope**

- This repository
- Obelisk-operated deployments of this relay
- Authentication, authorization, event integrity, data exposure, and denial-of-service issues

**Out of scope**

- Deployments operated by other people
- Vulnerabilities in Nostr itself — report those to the NIPs repository

## Please do not test against the live public relay

`wss://public.obelisk.ar` is used by real people. Run a local relay using `compose.yml` instead.

## What to include

- Impact and affected component
- Version or commit SHA
- Reproduction steps or proof of concept
- Any suggested fix

## Response times

- **Acknowledgement within 48 hours.**
- **Fix within 90 days** for high and critical severity.

I will credit you in the advisory unless you'd rather I didn't. There is no bug bounty; this is an
unfunded individual project.
