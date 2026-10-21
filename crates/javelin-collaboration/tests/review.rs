mod support;

use javelin_collaboration::{
    Action, AppliedPermissions, CommentBody, PrincipalId, Publishable, Refusal, RepositoryId,
    RequestState, Review, RevisionRef, Role, Visibility, check_publishable,
};
use support::*;

/// Publishes against the revision that is current now, as a publisher would.
fn publish_check(
    review: &Review,
    exact: &RevisionRef,
    p: &AppliedPermissions,
    publisher: &PrincipalId,
) -> Result<Publishable, Refusal> {
    let index = review.current().map_or(0, |c| c.index);
    check_publishable(review, index, exact, p, publisher)
}

/// Applies a command that must be accepted and returns the new state.
fn ok(
    review: &Review,
    command: javelin_collaboration::Command,
    p: &javelin_collaboration::AppliedPermissions,
) -> Review {
    let applied = review.apply(&command, p).unwrap();
    assert!(!applied.replayed);
    applied.review
}

/// A refusal must leave the original value untouched; `apply` takes `&self`.
fn refused(
    review: &Review,
    command: javelin_collaboration::Command,
    p: &javelin_collaboration::AppliedPermissions,
) -> Refusal {
    let before = review.clone();
    let refusal = review.apply(&command, p).unwrap_err();
    assert_eq!(*review, before);
    refusal
}

#[test]
fn repository_backed_proposal_flows_to_publishable() {
    let mut fx = Fixture::new();
    let rev = fx.proposal(1, "pub fn a() {}\n", Some(b"supplied trace bytes"));
    // The reference names real stored native objects.
    assert!(fx.repo.has_tree(rev.tree()).unwrap());
    assert_eq!(
        fx.repo.read_blob(rev.context().unwrap()).unwrap(),
        b"supplied trace bytes"
    );
    assert_eq!(rev.base().get(), 1);

    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(rev)), &p);
    assert_eq!(
        publish_check(&r, &rev, &p, &who("mona")),
        Err(Refusal::NotEnoughApprovals {
            required: 1,
            current: 0
        })
    );
    let r = ok(&r, cmd("a1", "rita", approve(1, rev)), &p);
    let allowed = publish_check(&r, &rev, &p, &who("mona")).unwrap();
    assert_eq!(allowed.index, 1);
    assert_eq!(allowed.approvals, vec![who("rita")]);
    // Only Maintain may Publish, even the approving reviewer or the author.
    for p_name in ["rita", "alice"] {
        assert_eq!(
            publish_check(&r, &rev, &p, &who(p_name)),
            Err(Refusal::InsufficientRole {
                required: Role::Maintain
            })
        );
    }
}

#[test]
fn only_author_submits_revisions_and_no_one_takes_over() {
    let mut fx = Fixture::new();
    let (r1, r2) = (fx.proposal(1, "one", None), fx.proposal(1, "two", None));
    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(r1)), &p);
    for actor in ["mona", "rita"] {
        assert_eq!(
            refused(&r, cmd("x", actor, revise(1, r2)), &p),
            Refusal::NotAuthor
        );
    }
    // A second Open cannot replace the author either.
    assert_eq!(
        refused(&r, cmd("x", "mona", open(r2)), &p),
        Refusal::AlreadyOpen
    );
    assert_eq!(r.author(), Some(&who("alice")));
    let r = ok(&r, cmd("v2", "alice", revise(1, r2)), &p);
    assert_eq!(r.revisions().len(), 2);
    assert_eq!(r.revisions()[0].revision, r1);
    assert_eq!(r.current().unwrap().revision, r2);
}

#[test]
fn changed_code_context_or_base_invalidates_approval() {
    let mut fx = Fixture::new();
    let code = fx.proposal(1, "same", Some(b"ctx-a"));
    let p = team(1);
    let base = ok(&review(), cmd("o1", "alice", open(code)), &p);
    let base = ok(&base, cmd("a1", "rita", approve(1, code)), &p);
    assert!(publish_check(&base, &code, &p, &who("mona")).is_ok());

    // Same code, different context; different code, same context; same code over World 0.
    let other_context = RevisionRef::new(
        code.base(),
        code.checkpoint(),
        code.tree(),
        Some(fx.repo.put_blob(b"ctx-b").unwrap()),
    );
    let no_context = RevisionRef::new(code.base(), code.checkpoint(), code.tree(), None);
    let other_code = fx.proposal(1, "changed", Some(b"ctx-a"));
    let other_base = fx.proposal(0, "same", Some(b"ctx-a"));
    assert_ne!(other_base.tree(), code.tree());
    for (n, next) in [other_context, no_context, other_code, other_base]
        .into_iter()
        .enumerate()
    {
        let r = ok(&base, cmd(&format!("v{n}"), "alice", revise(1, next)), &p);
        assert!(r.current().unwrap().approvals.is_empty());
        assert_eq!(
            publish_check(&r, &next, &p, &who("mona")),
            Err(Refusal::NotEnoughApprovals {
                required: 1,
                current: 0
            })
        );
        // The old exact revision is no longer publishable.
        assert_eq!(
            publish_check(&r, &code, &p, &who("mona")),
            Err(Refusal::RevisionMismatch)
        );
        // History retains the approved revision unchanged.
        assert_eq!(r.revisions()[0].approvals, vec![who("rita")]);
    }
}

#[test]
fn approval_binds_index_not_just_value() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(a)), &p);
    let r = ok(&r, cmd("ap", "rita", approve(1, a)), &p);
    let r = ok(&r, cmd("v2", "alice", revise(1, b)), &p);
    let r = ok(&r, cmd("v3", "alice", revise(2, a)), &p);
    assert_eq!(r.current().unwrap().index, 3);
    assert_eq!(
        publish_check(&r, &a, &p, &who("mona")),
        Err(Refusal::NotEnoughApprovals {
            required: 1,
            current: 0
        })
    );
    assert_eq!(
        refused(&r, cmd("ap1", "rita", approve(1, a)), &p),
        Refusal::StaleRevision { current: 3 }
    );
    assert_eq!(
        refused(&r, cmd("ap3", "rita", approve(3, b)), &p),
        Refusal::RevisionMismatch
    );
    assert_eq!(
        refused(&r, cmd("v4", "alice", revise(3, a)), &p),
        Refusal::UnchangedRevision
    );
    assert_eq!(
        refused(&r, cmd("v4", "alice", revise(2, b)), &p),
        Refusal::StaleRevision { current: 3 }
    );
}

#[test]
fn reviewer_rules_and_current_permission_evaluation() {
    let mut fx = Fixture::new();
    let rev = fx.proposal(1, "x", None);
    let p1 = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(rev)), &p1);
    assert_eq!(
        refused(&r, cmd("s", "alice", approve(1, rev)), &p1),
        Refusal::InsufficientRole {
            required: Role::Review
        }
    );
    // Even a maintainer author cannot self-approve.
    let p_author_maint = perms(
        1,
        Visibility::Private,
        &[
            ("alice", Role::Maintain),
            ("rita", Role::Review),
            ("mona", Role::Maintain),
        ],
        1,
    );
    assert_eq!(
        refused(&r, cmd("s", "alice", approve(1, rev)), &p_author_maint),
        Refusal::SelfApproval
    );
    let r = ok(&r, cmd("a1", "rita", approve(1, rev)), &p1);
    assert_eq!(
        refused(&r, cmd("a2", "rita", approve(1, rev)), &p1),
        Refusal::AlreadyApproved
    );

    // Revoke rita at epoch 2: her recorded approval no longer counts.
    let p2 = perms(
        2,
        Visibility::Private,
        &[("alice", Role::Read), ("mona", Role::Maintain)],
        1,
    );
    assert_eq!(
        publish_check(&r, &rev, &p2, &who("mona")),
        Err(Refusal::NotEnoughApprovals {
            required: 1,
            current: 0
        })
    );
    // Downgrade to Read also removes it; regrant restores it.
    let p3 = perms(
        3,
        Visibility::Private,
        &[
            ("alice", Role::Read),
            ("rita", Role::Read),
            ("mona", Role::Maintain),
        ],
        1,
    );
    assert!(publish_check(&r, &rev, &p3, &who("mona")).is_err());
    let p4 = perms(
        4,
        Visibility::Private,
        &[
            ("alice", Role::Read),
            ("rita", Role::Review),
            ("mona", Role::Maintain),
        ],
        1,
    );
    assert!(publish_check(&r, &rev, &p4, &who("mona")).is_ok());
    // Revoked publisher.
    let p5 = perms(
        5,
        Visibility::Private,
        &[("alice", Role::Read), ("rita", Role::Review)],
        1,
    );
    assert_eq!(
        publish_check(&r, &rev, &p5, &who("mona")),
        Err(Refusal::NoAccess)
    );
    // An author later granted Review still cannot count as their own approver.
    let p6 = perms(
        6,
        Visibility::Private,
        &[("alice", Role::Review), ("mona", Role::Maintain)],
        0,
    );
    assert_eq!(
        publish_check(&r, &rev, &p6, &who("mona"))
            .unwrap()
            .approvals,
        vec![]
    );
}

#[test]
fn stale_permission_epochs_and_foreign_repository_are_refused() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let r = ok(&review(), cmd("o1", "alice", open(a)), &team(5));
    assert_eq!(r.epoch(), 5);
    assert_eq!(
        refused(&r, cmd("v2", "alice", revise(1, b)), &team(4)),
        Refusal::StalePermissions { seen: 5 }
    );
    assert_eq!(
        publish_check(&r, &a, &team(4), &who("mona")),
        Err(Refusal::StalePermissions { seen: 5 })
    );
    // Equal epoch is accepted.
    let r = ok(&r, cmd("c", "rita", comment(1, "looks fine")), &team(5));
    let mut foreign = team(6);
    foreign.repository = RepositoryId::parse("repo-2").unwrap();
    assert_eq!(
        refused(&r, cmd("v2", "alice", revise(1, b)), &foreign),
        Refusal::RepositoryMismatch
    );
    assert_eq!(
        publish_check(&r, &a, &foreign, &who("mona")),
        Err(Refusal::RepositoryMismatch)
    );
}

#[test]
fn public_contributor_and_private_revocation() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let public = perms(
        1,
        Visibility::Public,
        &[("rita", Role::Review), ("mona", Role::Maintain)],
        1,
    );
    let r = ok(&review(), cmd("o1", "outsider", open(a)), &public);
    let r = ok(&r, cmd("c1", "passerby", comment(1, "nice")), &public);
    let r = ok(&r, cmd("v2", "outsider", revise(1, b)), &public);
    assert_eq!(
        refused(&r, cmd("a", "passerby", approve(2, b)), &public),
        Refusal::InsufficientRole {
            required: Role::Review
        }
    );
    let r = ok(&r, cmd("a", "rita", approve(2, b)), &public);
    assert_eq!(
        publish_check(&r, &b, &public, &who("outsider")),
        Err(Refusal::InsufficientRole {
            required: Role::Maintain
        })
    );
    assert!(publish_check(&r, &b, &public, &who("mona")).is_ok());

    // Repository becomes private: the outsider loses all access, history persists.
    let private = perms(
        2,
        Visibility::Private,
        &[("rita", Role::Review), ("mona", Role::Maintain)],
        1,
    );
    assert_eq!(
        refused(&r, cmd("v3", "outsider", revise(2, a)), &private),
        Refusal::NoAccess
    );
    assert_eq!(
        refused(&r, cmd("c2", "outsider", comment(2, "hi")), &private),
        Refusal::NoAccess
    );
    assert_eq!(r.revisions().len(), 2);
    assert_eq!(r.comments().len(), 1);
    // Private non-member cannot open a new request.
    assert_eq!(
        refused(&review(), cmd("o", "stranger", open(a)), &team(1)),
        Refusal::NoAccess
    );
}

#[test]
fn operation_ids_replay_exactly_and_refuse_different_input() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let p = team(1);
    let open_a = cmd("o1", "alice", open(a));
    let r = ok(&review(), open_a.clone(), &p);
    let r = ok(&r, cmd("a1", "rita", approve(1, a)), &p);

    // Identical retry replays with no change, even after alice is revoked at a later epoch.
    let revoked = perms(
        2,
        Visibility::Private,
        &[("rita", Role::Review), ("mona", Role::Maintain)],
        1,
    );
    let replay = r.apply(&open_a, &revoked).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.review, r);
    // Same ID with different revision, actor, or action kind is refused.
    for different in [
        cmd("o1", "alice", open(b)),
        cmd("o1", "mona", open(a)),
        cmd("o1", "alice", revise(1, b)),
        cmd("a1", "rita", comment(1, "x")),
    ] {
        assert_eq!(
            refused(&r, different.clone(), &p),
            Refusal::OperationIdReused(different.operation)
        );
    }
    // A refused command does not bind its ID; it is re-evaluated on retry.
    let early = cmd("c9", "carol", comment(1, "hello"));
    assert_eq!(refused(&r, early.clone(), &p), Refusal::NoAccess);
    let with_carol = perms(
        3,
        Visibility::Private,
        &[
            ("alice", Role::Read),
            ("rita", Role::Review),
            ("mona", Role::Maintain),
            ("carol", Role::Read),
        ],
        1,
    );
    let r = ok(&r, early.clone(), &with_carol);
    assert!(r.apply(&early, &with_carol).unwrap().replayed);
    assert_eq!(r.history().len(), 3);
}

#[test]
fn publish_access_refusal_does_not_reveal_revision_state() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let p = team(1);
    let empty = review();
    let opened = ok(&empty, cmd("o1", "alice", open(a)), &p);
    let approved = ok(&opened, cmd("a1", "rita", approve(1, a)), &p);

    for state in [&empty, &opened, &approved] {
        for exact in [a, b] {
            assert_eq!(
                publish_check(state, &exact, &p, &who("outsider")),
                Err(Refusal::NoAccess)
            );
            for actor in ["alice", "rita"] {
                assert_eq!(
                    publish_check(state, &exact, &p, &who(actor)),
                    Err(Refusal::InsufficientRole {
                        required: Role::Maintain
                    })
                );
            }
            let public = perms(1, Visibility::Public, &[], 0);
            assert_eq!(
                publish_check(state, &exact, &public, &who("outsider")),
                Err(Refusal::InsufficientRole {
                    required: Role::Maintain
                })
            );
        }
    }
}

#[test]
fn identical_replay_requires_same_repository_but_not_current_grants_or_epoch() {
    let mut fx = Fixture::new();
    let rev = fx.proposal(1, "a", None);
    let command = cmd("o1", "alice", open(rev));
    let r = ok(&review(), command.clone(), &team(5));

    for epoch in [0, 5, 6] {
        let same_repo = perms(epoch, Visibility::Private, &[], 0);
        let mut foreign = same_repo.clone();
        foreign.repository = RepositoryId::parse("repo-2").unwrap();
        assert_eq!(
            refused(&r, command.clone(), &foreign),
            Refusal::RepositoryMismatch
        );
        let replay = r.apply(&command, &same_repo).unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.review, r);
        assert_eq!(replay.review.epoch(), 5);
    }
}

#[test]
fn reordered_commands_refuse_then_succeed_in_order() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let p = team(1);
    let approve_b = cmd("ab", "rita", approve(2, b));
    let revise_b = cmd("vb", "alice", revise(1, b));
    let comment_2 = cmd("c2", "rita", comment(2, "on b"));
    // Before Open, everything is refused.
    for c in [&approve_b, &revise_b, &comment_2] {
        assert_eq!(refused(&review(), c.clone(), &p), Refusal::NotOpen);
    }
    let r = ok(&review(), cmd("o1", "alice", open(a)), &p);
    assert_eq!(
        refused(&r, approve_b.clone(), &p),
        Refusal::StaleRevision { current: 1 }
    );
    assert_eq!(refused(&r, comment_2.clone(), &p), Refusal::UnknownRevision);
    let r = ok(&r, revise_b, &p);
    let r = ok(&r, approve_b, &p);
    let r = ok(&r, comment_2, &p);
    assert!(publish_check(&r, &b, &p, &who("mona")).is_ok());
    // Two concurrent author revisions from the same observed index: second is stale.
    let c = fx.proposal(1, "c", None);
    let d = fx.proposal(1, "d", None);
    let r = ok(&r, cmd("vc", "alice", revise(2, c)), &p);
    assert_eq!(
        refused(&r, cmd("vd", "alice", revise(2, d)), &p),
        Refusal::StaleRevision { current: 3 }
    );
}

#[test]
fn comments_and_revisions_are_retained_in_order() {
    let mut fx = Fixture::new();
    let (a, b, c) = (
        fx.proposal(1, "a", None),
        fx.proposal(1, "b", None),
        fx.proposal(1, "c", None),
    );
    let p = team(1);
    let mut r = ok(&review(), cmd("o1", "alice", open(a)), &p);
    r = ok(&r, cmd("c1", "rita", comment(1, "first")), &p);
    r = ok(&r, cmd("v2", "alice", revise(1, b)), &p);
    let snapshot = r.clone();
    r = ok(
        &r,
        cmd("c2", "mona", comment(1, "late note on revision 1")),
        &p,
    );
    r = ok(&r, cmd("v3", "alice", revise(2, c)), &p);
    r = ok(
        &r,
        cmd("c3", "rita", request_comment("request-level note")),
        &p,
    );
    // Earlier states are values, unchanged by later transitions.
    assert_eq!(snapshot.comments().len(), 1);
    assert_eq!(snapshot.revisions().len(), 2);
    let bodies: Vec<_> = r
        .comments()
        .iter()
        .map(|c| (c.index, c.author.as_str(), c.body.as_str()))
        .collect();
    assert_eq!(
        bodies,
        [
            (Some(1), "rita", "first"),
            (Some(1), "mona", "late note on revision 1"),
            (None, "rita", "request-level note"),
        ]
    );
    let revs: Vec<_> = r
        .revisions()
        .iter()
        .map(|v| (v.index, v.revision))
        .collect();
    assert_eq!(revs, [(1, a), (2, b), (3, c)]);
    let ops: Vec<_> = r.history().iter().map(|c| c.operation.as_str()).collect();
    assert_eq!(ops, ["o1", "c1", "v2", "c2", "v3", "c3"]);
    assert_eq!(
        refused(&r, cmd("c0", "rita", comment(0, "x")), &p),
        Refusal::UnknownRevision
    );
    assert_eq!(
        refused(&r, cmd("c4", "rita", comment(4, "x")), &p),
        Refusal::UnknownRevision
    );
}

#[test]
fn identifiers_and_comments_are_validated_at_the_boundary() {
    for bad in ["", " alice", "alice\n", "a b", "é", &"x".repeat(129), "a/b"] {
        assert!(PrincipalId::parse(bad).is_err(), "{bad:?}");
    }
    assert!(PrincipalId::parse(&"x".repeat(128)).is_ok());
    assert!(PrincipalId::parse("user:42.dev_x-y").is_ok());
    assert!(CommentBody::parse("").is_err());
    assert!(CommentBody::parse(&"x".repeat(CommentBody::MAX_BYTES + 1)).is_err());
    assert!(CommentBody::parse(&"x".repeat(CommentBody::MAX_BYTES)).is_ok());
}

#[test]
fn optional_reviews_still_require_current_publish_role() {
    let mut fx = Fixture::new();
    let rev = fx.proposal(1, "a", None);
    let p = perms(1, Visibility::Private, &[("mona", Role::Maintain)], 0);
    let r = ok(&review(), cmd("o1", "mona", open(rev)), &p);
    assert_eq!(
        publish_check(&r, &rev, &p, &who("mona")).unwrap().approvals,
        vec![]
    );
    let empty = review();
    assert_eq!(
        publish_check(&empty, &rev, &p, &who("mona")),
        Err(Refusal::NotOpen)
    );
}

#[test]
fn base_only_change_is_a_different_revision() {
    let mut fx = Fixture::new();
    let on1 = fx.proposal(1, "same", Some(b"ctx"));
    let on2 = fx.proposal(2, "same", Some(b"ctx"));
    // Real native proposals with identical tree and context, different base.
    assert_eq!((on1.tree(), on1.context()), (on2.tree(), on2.context()));
    assert_ne!(on1.base(), on2.base());

    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(on1)), &p);
    assert_eq!(
        refused(&r, cmd("a2", "rita", approve(1, on2)), &p),
        Refusal::RevisionMismatch
    );
    let approved = ok(&r, cmd("a1", "rita", approve(1, on1)), &p);
    assert_eq!(
        publish_check(&approved, &on2, &p, &who("mona")),
        Err(Refusal::RevisionMismatch)
    );
    // Same operation ID and actor, base-only different input.
    assert_eq!(
        refused(&approved, cmd("o1", "alice", open(on2)), &p),
        Refusal::OperationIdReused(op("o1"))
    );
    // Rebased resubmission is accepted as a new revision and invalidates approval.
    let rebased = ok(&approved, cmd("v2", "alice", revise(1, on2)), &p);
    assert_eq!(
        publish_check(&rebased, &on2, &p, &who("mona")),
        Err(Refusal::NotEnoughApprovals {
            required: 1,
            current: 0
        })
    );
    assert_eq!(
        publish_check(&rebased, &on1, &p, &who("mona")),
        Err(Refusal::RevisionMismatch)
    );
}

#[test]
fn publish_pins_the_index_and_moves_the_request_to_published() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(a)), &p);
    let r = ok(&r, cmd("a1", "rita", approve(1, a)), &p);
    let r = ok(&r, cmd("v2", "alice", revise(1, b)), &p);
    // Revision 1 is approved but no longer current: it is not published.
    assert_eq!(
        check_publishable(&r, 1, &a, &p, &who("mona")),
        Err(Refusal::StaleRevision { current: 2 })
    );
    assert_eq!(
        refused(&r, cmd("p1", "mona", publish(1, a)), &p),
        Refusal::StaleRevision { current: 2 }
    );
    // Revision 2 exists but has no approval yet.
    assert_eq!(
        refused(&r, cmd("p2", "mona", publish(2, b)), &p),
        Refusal::NotEnoughApprovals {
            required: 1,
            current: 0
        }
    );
    let r = ok(&r, cmd("a2", "rita", approve(2, b)), &p);
    let published = ok(&r, cmd("p3", "mona", publish(2, b)), &p);
    assert_eq!(published.state(), RequestState::Published);
    // Published is terminal: nothing else is accepted.
    assert_eq!(
        refused(&published, cmd("c", "rita", request_comment("after")), &p),
        Refusal::NotOpen
    );
    assert_eq!(
        refused(&published, cmd("a3", "rita", approve(2, b)), &p),
        Refusal::NotOpen
    );
}

#[test]
fn close_refuses_a_revision_it_was_not_prepared_for() {
    let mut fx = Fixture::new();
    let a = fx.proposal(1, "a", None);
    let b = fx.proposal(2, "b", None);
    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(a)), &p);
    let r = ok(&r, cmd("v2", "alice", revise(1, b)), &p);
    assert_eq!(
        refused(&r, cmd("cl1", "alice", Action::Close { index: 1 }), &p),
        Refusal::StaleRevision { current: 2 }
    );
    let closed = ok(&r, cmd("cl2", "alice", Action::Close { index: 2 }), &p);
    assert_eq!(closed.state(), RequestState::Closed);
}

#[test]
fn closed_request_refuses_approval_and_publish() {
    let mut fx = Fixture::new();
    let rev = fx.proposal(1, "a", None);
    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(rev)), &p);
    let r = ok(&r, cmd("a1", "rita", approve(1, rev)), &p);
    // Only the author or a Maintain holder may close.
    assert_eq!(
        refused(&r, cmd("x", "rita", Action::Close { index: 1 }), &p),
        Refusal::InsufficientRole {
            required: Role::Maintain
        }
    );
    let closed = ok(&r, cmd("cl", "alice", Action::Close { index: 1 }), &p);
    assert_eq!(closed.state(), RequestState::Closed);
    // The approval exists, but the request is Closed.
    assert_eq!(
        refused(&closed, cmd("p", "mona", publish(1, rev)), &p),
        Refusal::NotOpen
    );
    assert_eq!(
        refused(&closed, cmd("a2", "rita", approve(1, rev)), &p),
        Refusal::NotOpen
    );
    assert_eq!(
        publish_check(&closed, &rev, &p, &who("mona")),
        Err(Refusal::NotOpen)
    );
}

#[test]
fn replay_rebuilds_the_same_state_without_rechecking_roles() {
    let mut fx = Fixture::new();
    let (a, b) = (fx.proposal(1, "a", None), fx.proposal(1, "b", None));
    let p = team(1);
    let r = ok(&review(), cmd("o1", "alice", open(a)), &p);
    let r = ok(&r, cmd("a1", "rita", approve(1, a)), &p);
    let r = ok(&r, cmd("v2", "alice", revise(1, b)), &p);
    let r = ok(&r, cmd("c", "mona", request_comment("note")), &p);
    // Rebuild after rita's grant is gone: past decisions keep their recorded epoch.
    let mut rebuilt = review();
    for command in r.history() {
        rebuilt = rebuilt.replay(command, 1).unwrap();
    }
    assert_eq!(rebuilt, r);
}
