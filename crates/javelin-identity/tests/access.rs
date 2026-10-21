use javelin_collaboration::{AppliedPermissions, PrincipalId, RepositoryId, Role, Visibility};
use javelin_identity::{Access, Claims, Issuer, Need, Refusal, Revocation, Scope, test_keys};

const NOW: u64 = 1_000_000;
const REPO: &str = "repo-1";

fn permissions(epoch: u64, visibility: Visibility, roles: &[(&str, Role)]) -> AppliedPermissions {
    AppliedPermissions {
        repository: RepositoryId::parse(REPO).unwrap(),
        epoch,
        visibility,
        roles: roles
            .iter()
            .map(|(who, role)| (PrincipalId::parse(who).unwrap(), *role))
            .collect(),
        required_approvals: 1,
    }
}

fn access(permissions: AppliedPermissions) -> Access {
    let mut access = Access::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    access.apply(permissions).unwrap();
    access
}

fn token(sub: &str, scopes: &[Scope], jti: &str) -> String {
    token_from(&test_keys::claims(sub, REPO, scopes, NOW, NOW + 600, jti))
}

fn token_from(claims: &Claims) -> String {
    test_keys::issuer().issue(claims).unwrap()
}

fn need(scope: Scope) -> Need<'static> {
    Need { scope, layer: None }
}

fn code_of(result: Result<impl std::fmt::Debug, Refusal>) -> &'static str {
    result.unwrap_err().code()
}

fn maintainer_and_reader() -> AppliedPermissions {
    permissions(
        1,
        Visibility::Private,
        &[("alice", Role::Maintain), ("bob", Role::Read)],
    )
}

#[test]
fn a_valid_token_for_a_role_is_granted_the_scopes_it_carries() {
    let access = access(maintainer_and_reader());
    let grant = access
        .authorize(
            Some(&token("alice", &[Scope::Publish], "tok-1")),
            NOW,
            need(Scope::Publish),
        )
        .unwrap();
    assert_eq!(grant.principal.as_deref(), Some("alice"));
    assert_eq!(grant.role, Role::Maintain);
}

#[test]
fn a_forged_token_is_unauthorized() {
    let access = access(maintainer_and_reader());
    let forged = Issuer::new(test_keys::KID, test_keys::signing_key(9))
        .unwrap()
        .issue(&test_keys::claims(
            "alice",
            REPO,
            &[Scope::Publish],
            NOW,
            NOW + 600,
            "tok-1",
        ))
        .unwrap();
    assert_eq!(
        code_of(access.authorize(Some(&forged), NOW, need(Scope::Publish))),
        "unauthorized"
    );
}

#[test]
fn an_expired_token_is_unauthorized() {
    let access = access(maintainer_and_reader());
    let old = token("alice", &[Scope::Publish], "tok-1");
    assert_eq!(
        code_of(access.authorize(Some(&old), NOW + 600, need(Scope::Publish))),
        "unauthorized"
    );
}

#[test]
fn a_revoked_token_is_refused_once_the_push_is_applied() {
    let mut access = access(maintainer_and_reader());
    let revoked = token("alice", &[Scope::Publish], "tok-revoked");
    let kept = token("alice", &[Scope::Publish], "tok-kept");
    assert!(
        access
            .authorize(Some(&revoked), NOW, need(Scope::Publish))
            .is_ok()
    );
    access.revoke(Revocation::Token("tok-revoked".into()));
    assert_eq!(
        code_of(access.authorize(Some(&revoked), NOW, need(Scope::Publish))),
        "unauthorized"
    );
    assert!(
        access
            .authorize(Some(&kept), NOW, need(Scope::Publish))
            .is_ok()
    );
}

#[test]
fn a_revoked_device_refuses_every_token_it_issued() {
    let mut access = access(maintainer_and_reader());
    let mut other = test_keys::claims("alice", REPO, &[Scope::Publish], NOW, NOW + 600, "tok-2");
    other.dev = "dev-2".into();
    let phone = token_from(&other);
    access.revoke(Revocation::Device("dev-1".into()));
    assert_eq!(
        code_of(access.authorize(
            Some(&token("alice", &[Scope::Publish], "tok-1")),
            NOW,
            need(Scope::Publish)
        )),
        "unauthorized"
    );
    assert!(
        access
            .authorize(Some(&phone), NOW, need(Scope::Publish))
            .is_ok()
    );
}

#[test]
fn a_token_that_names_fewer_scopes_than_the_role_allows_is_forbidden() {
    let access = access(maintainer_and_reader());
    let read_only = token("alice", &[Scope::Read], "tok-1");
    assert_eq!(
        code_of(access.authorize(Some(&read_only), NOW, need(Scope::Publish))),
        "forbidden"
    );
}

#[test]
fn a_role_that_does_not_allow_a_scope_forbids_it_even_when_the_token_carries_it() {
    let access = access(maintainer_and_reader());
    let overreach = token("bob", &[Scope::Publish], "tok-1");
    assert_eq!(
        code_of(access.authorize(Some(&overreach), NOW, need(Scope::Publish))),
        "forbidden"
    );
}

#[test]
fn approval_needs_review_and_publish_needs_maintain() {
    let access = access(permissions(
        1,
        Visibility::Private,
        &[("rita", Role::Review)],
    ));
    let reviewer = token("rita", &[Scope::ReviewApprove, Scope::Publish], "tok-1");
    assert!(
        access
            .authorize(Some(&reviewer), NOW, need(Scope::ReviewApprove))
            .is_ok()
    );
    assert_eq!(
        code_of(access.authorize(Some(&reviewer), NOW, need(Scope::Publish))),
        "forbidden"
    );
}

#[test]
fn a_reader_may_author_requests_but_not_approve_them() {
    let access = access(maintainer_and_reader());
    let author = token(
        "bob",
        &[
            Scope::Read,
            Scope::LayerWrite,
            Scope::ReviewWrite,
            Scope::ReviewApprove,
        ],
        "tok-1",
    );
    assert!(
        access
            .authorize(Some(&author), NOW, need(Scope::ReviewWrite))
            .is_ok()
    );
    assert!(
        access
            .authorize(Some(&author), NOW, need(Scope::LayerWrite))
            .is_ok()
    );
    assert_eq!(
        code_of(access.authorize(Some(&author), NOW, need(Scope::ReviewApprove))),
        "forbidden"
    );
}

#[test]
fn repo_create_is_never_granted_to_a_repository_role() {
    let access = access(maintainer_and_reader());
    let create = token("alice", &[Scope::RepoCreate], "tok-1");
    assert_eq!(
        code_of(access.authorize(Some(&create), NOW, need(Scope::RepoCreate))),
        "forbidden"
    );
}

#[test]
fn a_restricted_resource_without_a_role_is_forbidden() {
    let access = access(maintainer_and_reader());
    let stranger = token("carol", &[Scope::Read], "tok-1");
    assert_eq!(
        code_of(access.authorize(Some(&stranger), NOW, need(Scope::Read))),
        "forbidden"
    );
}

#[test]
fn a_public_repository_gives_any_signed_in_caller_read_only() {
    let access = access(permissions(
        1,
        Visibility::Public,
        &[("alice", Role::Maintain)],
    ));
    let stranger = token("carol", &[Scope::Read, Scope::Publish], "tok-1");
    let grant = access
        .authorize(Some(&stranger), NOW, need(Scope::Read))
        .unwrap();
    assert_eq!(grant.role, Role::Read);
    assert_eq!(
        code_of(access.authorize(Some(&stranger), NOW, need(Scope::Publish))),
        "forbidden"
    );
}

#[test]
fn anonymous_callers_read_only_public_repositories() {
    let public = access(permissions(1, Visibility::Public, &[]));
    let grant = public.authorize(None, NOW, need(Scope::Read)).unwrap();
    assert_eq!(grant.principal, None);
    assert_eq!(
        code_of(public.authorize(None, NOW, need(Scope::Publish))),
        "unauthorized"
    );
    let private = access(permissions(1, Visibility::Private, &[]));
    assert_eq!(
        code_of(private.authorize(None, NOW, need(Scope::Read))),
        "unauthorized"
    );
}

#[test]
fn a_token_for_another_repository_or_a_discovery_token_is_forbidden() {
    let access = access(maintainer_and_reader());
    for repo in ["repo-2", "*"] {
        let claims = test_keys::claims("alice", repo, &[Scope::Publish], NOW, NOW + 600, "tok-1");
        assert_eq!(
            code_of(access.authorize(Some(&token_from(&claims)), NOW, need(Scope::Publish))),
            "forbidden",
            "repo {repo}"
        );
    }
}

#[test]
fn nothing_is_granted_before_permissions_are_applied() {
    let access = Access::new(RepositoryId::parse(REPO).unwrap(), test_keys::keys());
    assert_eq!(access.epoch(), None);
    assert_eq!(
        code_of(access.authorize(
            Some(&token("alice", &[Scope::Read], "tok-1")),
            NOW,
            need(Scope::Read)
        )),
        "forbidden"
    );
}

#[test]
fn a_token_limited_to_one_layer_is_forbidden_on_any_other_layer() {
    let access = access(permissions(
        1,
        Visibility::Private,
        &[("agent", Role::Read)],
    ));
    let mut claims =
        test_keys::claims("agent", REPO, &[Scope::LayerWrite], NOW, NOW + 600, "tok-1");
    claims.layer = Some("layer-a".into());
    let limited = token_from(&claims);
    let on_a = Need {
        scope: Scope::LayerWrite,
        layer: Some("layer-a"),
    };
    let on_b = Need {
        scope: Scope::LayerWrite,
        layer: Some("layer-b"),
    };
    assert!(access.authorize(Some(&limited), NOW, on_a).is_ok());
    assert_eq!(
        code_of(access.authorize(Some(&limited), NOW, on_b)),
        "forbidden"
    );
    assert_eq!(
        code_of(access.authorize(Some(&limited), NOW, need(Scope::LayerWrite))),
        "forbidden"
    );
}

#[test]
fn a_stale_epoch_is_ignored_and_a_newer_epoch_applies() {
    let mut access = access(maintainer_and_reader());
    let alice = token("alice", &[Scope::Publish], "tok-1");
    assert!(
        access
            .authorize(Some(&alice), NOW, need(Scope::Publish))
            .is_ok()
    );

    access
        .apply(permissions(
            2,
            Visibility::Private,
            &[("alice", Role::Read)],
        ))
        .unwrap();
    assert_eq!(
        code_of(access.authorize(Some(&alice), NOW, need(Scope::Publish))),
        "forbidden"
    );

    let replay = access.apply(maintainer_and_reader());
    assert_eq!(replay, Err(Refusal::StalePermissions { seen: 2 }));
    assert_eq!(
        code_of(access.authorize(Some(&alice), NOW, need(Scope::Publish))),
        "forbidden"
    );

    access
        .apply(permissions(
            3,
            Visibility::Private,
            &[("alice", Role::Maintain)],
        ))
        .unwrap();
    assert!(
        access
            .authorize(Some(&alice), NOW, need(Scope::Publish))
            .is_ok()
    );
    assert_eq!(access.epoch(), Some(3));
}

#[test]
fn the_applied_epoch_is_idempotent_only_when_identical() {
    let mut access = access(maintainer_and_reader());
    assert_eq!(access.apply(maintainer_and_reader()), Ok(()));
    let changed = permissions(1, Visibility::Private, &[("alice", Role::Read)]);
    assert_eq!(
        access.apply(changed),
        Err(Refusal::StalePermissions { seen: 1 })
    );
}

#[test]
fn permissions_for_another_repository_are_refused() {
    let mut access = access(maintainer_and_reader());
    let mut other = maintainer_and_reader();
    other.epoch = 2;
    other.repository = RepositoryId::parse("repo-2").unwrap();
    assert!(matches!(access.apply(other), Err(Refusal::Forbidden(_))));
    assert_eq!(access.epoch(), Some(1));
}

#[test]
fn refusals_display_their_code_first() {
    assert_eq!(
        Refusal::Unauthorized("token has expired").to_string(),
        "unauthorized: token has expired"
    );
    assert_eq!(
        Refusal::StalePermissions { seen: 4 }.to_string(),
        "stale_permissions: epoch 4 was already applied"
    );
}
