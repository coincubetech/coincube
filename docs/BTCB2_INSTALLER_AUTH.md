# Authenticated installer startup

The BTCB2 installer retains the authenticated Connect client after either a new
OTP login or adoption of an existing session. Its configured endpoint is kept;
the current token is supplied only to the ephemeral runtime admission context.
Persisted fork daemon configuration contains endpoint selection without JWTs.

Before authenticated startup, fork path preparation computes an absolute path
without creating a network or wallet directory. The embedded daemon performs
its authenticated anchor/provider admission before its own directory/database
writes. Installer configuration and encrypted seed persistence follow that
successful startup check. Bitcoin retains its existing directory preparation
and daemon startup path.

This change depends on the authenticated embedded client in PR430 and daemon
admission/cleanup in PR428. The explicit Connect capability also rechecks the
current account feature flag before admission. Generic runtime support remains
dormant; Claim, external signing, local nodes and Lightning SDKs stay closed.

Synthetic regression checks cover new OTP and retained-session handoff, fork
path preparation without writes with a Bitcoin positive control, and missing
startup client refusal without wallet writes. Live server admission is pending
infrastructure endpoint readiness and separate correct-chain acceptance; local
fixtures do not satisfy that gate.
