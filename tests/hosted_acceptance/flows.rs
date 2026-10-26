//! Public and private repository flows, permissions and retries.

use super::support::*;

// ---------------------------------------------------------------------------------------------
// Public and private repository flows.

#[test]
fn public_and_private_repository_flows_permissions_and_retries() {
    let hosted = Hosted::start();
    verified(
        "flow.browser-device-approval",
        "device_start, passkey enroll and sign-in, then POST /device/approve with the cookie over \
         loopback HTTP; another Origin is refused (403); device_poll returns the session",
    );
    let plane = hosted.plane();
    let bearer = &hosted.session.access_token;

    let site = hosted.create("site", true, Publish::Requests);
    let vault = hosted.create("vault", false, Publish::Direct);
    assert!(site.created && site.public && site.publish == Publish::Requests);
    assert!(vault.created && !vault.public && vault.publish == Publish::Direct);
    assert_eq!((site.epoch, vault.epoch), (1, 1));
    assert_eq!(head(&hosted.node_dir(&site.repository)), v(0));
    assert_eq!(head(&hosted.node_dir(&vault.repository)), v(0));

    // Retries: the same operation is the same repository; anything else is refused.
    let again = plane
        .create_repository(
            bearer,
            &create_request("site", true, Publish::Requests),
            now(),
        )
        .unwrap();
    assert!(!again.created);
    assert_eq!(again.repository, site.repository);
    let mut collision = create_request("site", true, Publish::Requests);
    collision.operation = "create-site-again".into();
    assert_eq!(
        plane
            .create_repository(bearer, &collision, now())
            .unwrap_err()
            .code(),
        "conflict"
    );
    let mut reused = create_request("other", true, Publish::Requests);
    reused.operation = "create-site".into();
    assert_eq!(
        plane
            .create_repository(bearer, &reused, now())
            .unwrap_err()
            .code(),
        "operation_id_reused"
    );
    verified(
        "flow.retries",
        "repeating create with its operation ID returns the same repository (created=false); a \
         name collision is conflict; reusing an operation ID for another request is \
         operation_id_reused",
    );

    // A refreshed sign-in gets repository tokens; a repository token cannot create.
    let fresh = plane.refresh(&hosted.session.refresh_token, now()).unwrap();
    let site_token = plane
        .repository_token(&fresh.access_token, &site.repository, now())
        .unwrap()
        .access_token;
    let vault_token = hosted.token(&vault.repository);
    assert_eq!(
        plane
            .create_repository(
                &site_token,
                &create_request("x", false, Publish::Direct),
                now()
            )
            .unwrap_err()
            .code(),
        "forbidden"
    );

    // What the node holds after provisioning: visibility, and the creator as the only role.
    let site_gate = hosted.gate(&site.repository);
    let vault_gate = hosted.gate(&vault.repository);
    for (gate, visibility) in [
        (&site_gate, Visibility::Public),
        (&vault_gate, Visibility::Private),
    ] {
        let applied = gate.permissions().unwrap();
        assert_eq!(applied.epoch, 1);
        assert_eq!(applied.visibility, visibility);
        assert_eq!(
            applied
                .roles
                .keys()
                .map(PrincipalId::as_str)
                .collect::<Vec<_>>(),
            ["admin"]
        );
    }
    let need = |scope| Need { scope, layer: None };
    let t = now();

    // Anonymous: a public repository is readable, nothing more; a private one is not.
    site_gate.check(None, t, need(Scope::Read)).unwrap();
    assert_eq!(
        code_of(site_gate.check(None, t, need(Scope::LayerWrite))),
        "unauthorized"
    );
    assert_eq!(
        code_of(vault_gate.check(None, t, need(Scope::Read))),
        "unauthorized"
    );
    verified(
        "flow.public-private",
        "public+requests and private+direct repositories created through the control plane and \
         provisioned at World 0; anonymous Read of the public one is allowed and nothing else; \
         anonymous Read of the private one is unauthorized",
    );

    // Tokens: exact repository, role intersected with scope, epochs enforced.
    vault_gate
        .check(Some(&vault_token), t, need(Scope::Publish))
        .unwrap();
    assert_eq!(
        code_of(vault_gate.check(Some(&site_token), t, need(Scope::Read))),
        "forbidden",
        "a token for another repository"
    );
    assert_eq!(
        code_of(vault_gate.check(Some(bearer), t, need(Scope::Read))),
        "forbidden",
        "a sign-in (`*`) token at a node"
    );
    hosted.push(&vault, 2, &[("bob", Role::Read)], 0);
    let vault_gate = hosted.gate(&vault.repository);
    let bob = member(&vault.repository, "bob", &[Scope::Read, Scope::Publish]);
    vault_gate.check(Some(&bob), t, need(Scope::Read)).unwrap();
    assert_eq!(
        code_of(vault_gate.check(Some(&bob), t, need(Scope::Publish))),
        "forbidden",
        "a Read role with a publish-scoped token"
    );
    let stranger = member(&vault.repository, "eve", &[Scope::Read]);
    assert_eq!(
        code_of(vault_gate.check(Some(&stranger), t, need(Scope::Read))),
        "forbidden",
        "no role in the private repository"
    );
    // A stale push is refused; the same push again is a harmless retry.
    let stale = hosted
        .nodes()
        .apply(&permissions(&vault, 1, &[("eve", Role::Maintain)], 0))
        .unwrap_err();
    assert_eq!(stale.code(), "stale_permissions");
    hosted.push(&vault, 2, &[("bob", Role::Read)], 0);
    assert!(
        hosted
            .gate(&vault.repository)
            .check(Some(&stranger), t, need(Scope::Read))
            .is_err()
    );
    verified(
        "flow.permissions",
        "control-plane tokens are bound to one repository and refused elsewhere (forbidden); a \
         `*` sign-in token is refused at a node; role caps scope (Read role cannot publish); a \
         stale permissions push is stale_permissions and changes nothing",
    );

    // The node's wire: every message passes the gate with the request's bearer token.
    let (addr, server) = serve_then_return(
        hosted.log(&site.repository),
        hosted.gate(&site.repository),
        &site.repository,
        2,
    );
    let resolved = call(addr, &site.repository, &Message::Resolve { version: None }).unwrap();
    let anonymous_write = call(
        addr,
        &site.repository,
        &Message::PublishLayer {
            operation: op("wire-publish"),
            layer: layer("absent"),
            expected: ObjectId::from_bytes([0; 32]),
            base: v(0),
            attribution: PublishAttribution::Human,
        },
    )
    .unwrap();
    server.join().unwrap();
    assert!(matches!(resolved, Message::Resolved { number, .. } if number == v(0)));
    assert!(matches!(anonymous_write, Message::Error { ref code, .. } if code == "unauthorized"));
    let (addr, server) = serve_then_return(
        hosted.log(&vault.repository),
        hosted.gate(&vault.repository),
        &vault.repository,
        4,
    );
    let resolve = Message::Resolve { version: None };
    let anonymous = call(addr, &vault.repository, &resolve).unwrap();
    let elsewhere = call_as(
        addr,
        &vault.repository,
        Some(&hosted.token(&site.repository)),
        &resolve,
    )
    .unwrap();
    let signin = call_as(
        addr,
        &vault.repository,
        Some(&hosted.session.access_token),
        &resolve,
    )
    .unwrap();
    let creator = call_as(
        addr,
        &vault.repository,
        Some(&hosted.token(&vault.repository)),
        &resolve,
    )
    .unwrap();
    server.join().unwrap();
    let code = |reply: &Message| match reply {
        Message::Error { code, .. } => code.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    };
    assert_eq!(code(&anonymous), "unauthorized");
    assert_eq!(code(&elsewhere), "forbidden");
    assert_eq!(code(&signin), "forbidden");
    assert!(matches!(creator, Message::Resolved { .. }));
    verified(
        "flow.wire-authentication",
        "over the node's loopback HTTP, with Authorization: Bearer: an anonymous read of the \
         public repository succeeds and an anonymous write is unauthorized; the private \
         repository refuses an anonymous read (unauthorized), another repository's token and the \
         `*` sign-in token (forbidden), and serves the creator's repository token",
    );
    not_verified(
        "flow.membership",
        "the control plane has no membership API (repository_token serves the creator and admin \
         only); members here are pushed to the node directly with test-key tokens",
    );
}
