//! Ending what an account already has open.
//!
//! Refusing the *next* request is the easy half of taking access away, and it
//! is the half the server already did: `auth::resolve::bearer` re-reads
//! `disabled` per request, and so does `auth::cert::client_cert`. The other
//! half is everything that is already connected, and before R-01 H5 there was
//! none of it — a disabled account's EUD kept its `:8089` session, kept
//! receiving every reachable peer's position and kept injecting CoT until its
//! TCP connection happened to drop, and its open `GET /api/v1/events` response
//! kept delivering.
//!
//! # Why it is one function
//!
//! Three things end a session and they are in three different modules: the
//! refresh family is storage, the CoT connection is
//! [`LiveState`](crate::stream::LiveState), and the event feed is an HTTP
//! response nothing holds a handle to. A caller that has just decided somebody
//! may no longer be here should not have to remember all three, and the two
//! places that decide it — disabling an account and revoking a credential —
//! must not drift apart.
//!
//! # What it does not do
//!
//! It does not revoke the account's access tokens by `jti`. It does not need
//! to: every path that accepts one re-reads the account on the request, so an
//! access token for a disabled account is already refused. Listing each live
//! `jti` would put rows in `revoked_jtis` for tokens nothing would honour.

use crate::db::repos::UserRow;
use crate::prelude::*;

/// Ends every session an account holds, as far as this process can reach.
///
/// Best-effort by design: this is called *after* the decision has been recorded
/// in storage, so a failure here must not turn "the account is disabled" into
/// an error the operator reads as "nothing happened". Each failure is logged
/// and recorded on the session.
///
/// Answers how many live stream connections were closed, which is what the
/// audit entry and the log line report.
///
/// Takes the concrete [`AppContext`] rather than `&impl Services` because
/// `live()` is an inherent method on it: the stream registry is installed by
/// the listener at start-up rather than being part of the storage handle.
#[instrument("identity.sessions.end", skip_all, fields(username = %user.username))]
pub async fn end_all(services: &AppContext, user: &UserRow) -> usize {
    // First, because it is the only one that survives a restart: a client
    // holding a refresh token would otherwise mint fresh access tokens for as
    // long as the family lives.
    if let Err(err) = services
        .db()
        .refresh_tokens()
        .revoke_all_for_user(user.id)
        .await
    {
        warn!(error = %err, "Could not revoke the refresh tokens of an account being cut off.");
        services.session().record_human_error(&err);
    }

    // Open server-event feeds re-authorize on this rather than waiting for
    // their periodic check.
    services.events().invalidate(&user.username);

    if !services.has_live() {
        return 0;
    }

    match services.live() {
        Ok(live) => live.disconnect_by_user(&user.username),
        Err(err) => {
            warn!(error = %err, "Could not reach the stream registry to close a session.");
            services.session().record_human_error(&err);

            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::tokens;
    use crate::testing::TestServer;

    #[tokio::test]
    async fn cutting_an_account_off_takes_its_refresh_family_with_it() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;

        end_all(&server.context, &user).await;

        assert!(
            tokens::rotate(&server.context, &session.refresh_token.unwrap(), None)
                .await
                .is_err(),
            "a refresh family that outlives the account mints access tokens forever",
        );
    }

    #[tokio::test]
    async fn an_installation_with_no_stream_listener_is_not_an_error() {
        // The state a fresh installation and every `[stream.tls] enabled=false`
        // deployment is in; disabling an account there must still work.
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;

        assert_eq!(end_all(&server.context, &user).await, 0);
    }

    #[tokio::test]
    async fn an_open_feed_is_told_to_re_authorize() {
        let server = TestServer::start().await;
        let user = server.user("ada", false).await;
        let mut invalidations = server.context.events().invalidations();

        end_all(&server.context, &user).await;

        assert_eq!(invalidations.recv().await.unwrap(), user.username);
    }
}
