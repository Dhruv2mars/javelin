use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Claims, Need, Revocation, Scope, test_keys};
use javelin_node::auth::Gate;
use javelin_protocol::{ProtocolError, codec::code};

const NOW: u64 = 1_000_000;
const REPO: &str = "repo-1";

fn permissions(epoch: u64, roles: &[(&str, Role)]) -> AppliedPermissions {
    AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch,
        visibility: Visibility::Private,
        roles: roles
            .iter()
            .map(|(who, role)| (PrincipalId::parse(who).unwrap(), *role))
            .collect(),
        required_approvals: 1,
    }
}

fn gate() -> Gate {
    let mut gate = Gate::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    gate.apply(permissions(
        1,
        &[("alice", Role::Maintain), ("bob", Role::Read)],
    ))
    .unwrap();
    gate
}

fn claims(sub: &str, scopes: &[Scope], iat: u64, exp: u64, jti: &str) -> Claims {
    test_keys::claims(sub, REPO, scopes, iat, exp, jti)
}

fn issue(claims: &Claims) -> String {
    test_keys::issuer().issue(claims).unwrap()
}

fn publish() -> Need<'static> {
    Need {
        scope: Scope::Publish,
        layer: None,
    }
}

fn refusal(result: Result<impl std::fmt::Debug, ProtocolError>) -> String {
    result.unwrap_err().code
}

#[test]
fn a_forged_token_is_unauthorized() {
    let forged = javelin_identity::Issuer::new(test_keys::KID, test_keys::signing_key(9))
        .unwrap()
        .issue(&claims("alice", &[Scope::Publish], NOW, NOW + 600, "tok-1"))
        .unwrap();
    assert_eq!(
        refusal(gate().check(Some(&forged), NOW, publish())),
        code::UNAUTHORIZED
    );
}

#[test]
fn an_expired_token_is_unauthorized() {
    let old = issue(&claims("alice", &[Scope::Publish], NOW, NOW + 600, "tok-1"));
    assert_eq!(
        refusal(gate().check(Some(&old), NOW + 601, publish())),
        code::UNAUTHORIZED
    );
}

#[test]
fn a_revoked_token_is_refused_once_the_push_is_applied() {
    let mut gate = gate();
    let token = issue(&claims("alice", &[Scope::Publish], NOW, NOW + 600, "tok-1"));
    assert!(gate.check(Some(&token), NOW, publish()).is_ok());
    gate.revoke(Revocation::Token("tok-1".into()));
    assert_eq!(
        refusal(gate.check(Some(&token), NOW, publish())),
        code::UNAUTHORIZED
    );
}

#[test]
fn a_token_narrower_than_its_role_cannot_publish() {
    let read_only = issue(&claims("alice", &[Scope::Read], NOW, NOW + 600, "tok-1"));
    assert_eq!(
        refusal(gate().check(Some(&read_only), NOW, publish())),
        code::FORBIDDEN
    );
}

#[test]
fn a_restricted_resource_without_a_role_is_forbidden() {
    let stranger = issue(&claims("carol", &[Scope::Read], NOW, NOW + 600, "tok-1"));
    assert_eq!(
        refusal(gate().check(
            Some(&stranger),
            NOW,
            Need {
                scope: Scope::Read,
                layer: None
            }
        )),
        code::FORBIDDEN
    );
}

#[test]
fn a_stale_epoch_push_is_ignored_and_a_newer_one_applies() {
    let mut gate = gate();
    let alice = issue(&claims("alice", &[Scope::Publish], NOW, NOW + 600, "tok-1"));
    gate.apply(permissions(2, &[("alice", Role::Read)]))
        .unwrap();

    let stale = gate.apply(permissions(1, &[("alice", Role::Maintain)]));
    assert_eq!(refusal(stale), code::STALE_PERMISSIONS);
    assert_eq!(
        refusal(gate.check(Some(&alice), NOW, publish())),
        code::FORBIDDEN
    );

    gate.apply(permissions(3, &[("alice", Role::Maintain)]))
        .unwrap();
    assert!(gate.check(Some(&alice), NOW, publish()).is_ok());
}

#[test]
fn a_valid_token_keeps_working_while_the_control_plane_is_down() {
    // The issuer stands in for the control plane. Once the node holds the keys and the
    // permissions, nothing it does needs the issuer again.
    let issuer = test_keys::issuer();
    let token = issuer
        .issue(&claims(
            "alice",
            &[Scope::Publish],
            NOW,
            NOW + 3600,
            "tok-1",
        ))
        .unwrap();
    drop(issuer);
    assert!(gate().check(Some(&token), NOW + 1800, publish()).is_ok());
    assert_eq!(
        refusal(gate().check(Some(&token), NOW + 3600, publish())),
        code::UNAUTHORIZED
    );
}

#[test]
fn an_anonymous_caller_reads_only_a_public_repository() {
    let mut gate = gate();
    let read = Need {
        scope: Scope::Read,
        layer: None,
    };
    assert_eq!(refusal(gate.check(None, NOW, read)), code::UNAUTHORIZED);
    let mut public = permissions(2, &[]);
    public.visibility = Visibility::Public;
    gate.apply(public).unwrap();
    assert!(gate.check(None, NOW, read).is_ok());
    assert_eq!(
        refusal(gate.check(None, NOW, publish())),
        code::UNAUTHORIZED
    );
}

#[test]
fn a_rotated_key_is_trusted_only_while_it_stays_in_the_set() {
    let mut gate = gate();
    let next = javelin_identity::Issuer::new(test_keys::NEXT_KID, test_keys::signing_key(2))
        .unwrap()
        .issue(&claims("alice", &[Scope::Publish], NOW, NOW + 600, "tok-2"))
        .unwrap();
    assert_eq!(
        refusal(gate.check(Some(&next), NOW, publish())),
        code::UNAUTHORIZED
    );
    gate.keys_mut()
        .trust(
            test_keys::NEXT_KID,
            test_keys::signing_key(2).verifying_key(),
        )
        .unwrap();
    assert!(gate.check(Some(&next), NOW, publish()).is_ok());
}
