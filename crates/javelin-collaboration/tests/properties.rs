//! Deterministic randomized command sequences checked against state invariants.

mod support;

use javelin_collaboration::{
    Action, AppliedPermissions, Command, RequestState, Review, RevisionRef, Role, Visibility,
    check_publishable,
};
use support::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

const PEOPLE: [&str; 4] = ["alice", "rita", "mona", "out"];
const ROLES: [Option<Role>; 4] = [
    None,
    Some(Role::Read),
    Some(Role::Review),
    Some(Role::Maintain),
];

fn random_perms(rng: &mut Rng, epoch: u64) -> AppliedPermissions {
    let visibility = if rng.below(2) == 0 {
        Visibility::Public
    } else {
        Visibility::Private
    };
    let roles: Vec<(&str, Role)> = PEOPLE
        .iter()
        .filter_map(|p| ROLES[rng.below(4)].map(|r| (*p, r)))
        .collect();
    perms(epoch, visibility, &roles, rng.below(3) as u32)
}

fn random_command(
    rng: &mut Rng,
    state: &Review,
    revs: &[RevisionRef],
    ops: &mut Vec<Command>,
    n: usize,
) -> Command {
    // Sometimes retry an earlier command verbatim, or reuse its ID with new input.
    if !ops.is_empty() && rng.below(5) == 0 {
        let prior = ops[rng.below(ops.len())].clone();
        if rng.below(2) == 0 {
            return prior;
        }
        let mut changed = random_command(rng, state, revs, &mut Vec::new(), n);
        changed.operation = prior.operation;
        return changed;
    }
    // Mostly target the current revision so many commands are accepted.
    let current = state.current();
    let index = match current {
        Some(c) if rng.below(4) != 0 => c.index,
        _ => rng.below(5) as u32,
    };
    let rev = match current {
        Some(c) if rng.below(3) == 0 => c.revision,
        _ => revs[rng.below(revs.len())],
    };
    // Publish and Close end a request, so they stay rare to keep long runs.
    let action = match rng.below(50) {
        0 => open(rev),
        1..=14 => revise(index, rev),
        15..=28 => approve(index, rev),
        29..=38 => comment(index, "note"),
        39..=44 => request_comment("note"),
        45 => publish(index, rev),
        46 => Action::Close { index },
        _ => comment(index, "note"),
    };
    let c = cmd(&format!("op-{n}"), PEOPLE[rng.below(PEOPLE.len())], action);
    ops.push(c.clone());
    c
}

fn check_invariants(before: &Review, after: &Review) {
    // Author is fixed once set; revisions and comments only append.
    if before.author().is_some() {
        assert_eq!(before.author(), after.author());
    }
    let n = before.revisions().len();
    assert!(after.revisions().len() >= n);
    if after.revisions().len() > n {
        assert_eq!(&after.revisions()[..n], before.revisions());
        assert!(after.current().unwrap().approvals.is_empty());
    } else if n > 0 {
        assert_eq!(&after.revisions()[..n - 1], &before.revisions()[..n - 1]);
        assert!(
            after.revisions()[n - 1]
                .approvals
                .starts_with(&before.revisions()[n - 1].approvals)
        );
    }
    assert!(after.comments().starts_with(before.comments()));
    assert!(after.history().starts_with(before.history()));
    for (i, v) in after.revisions().iter().enumerate() {
        assert_eq!(v.index as usize, i + 1);
        assert!(after.author().is_some_and(|a| !v.approvals.contains(a)));
    }
    for (i, c) in after.history().iter().enumerate() {
        assert!(
            after.history()[..i]
                .iter()
                .all(|p| p.operation != c.operation)
        );
        if matches!(c.action, Action::Revise { .. }) {
            assert_eq!(Some(&c.actor), after.author());
        }
    }
}

fn run(seed: u64, revs: &[RevisionRef]) -> Review {
    let mut rng = Rng(seed);
    let mut ops = Vec::new();
    let mut epoch = 1;
    let mut p = random_perms(&mut rng, epoch);
    let mut state = review();
    for n in 0..120 {
        if rng.below(6) == 0 {
            // Mostly advance; occasionally supply a regressed epoch.
            epoch = if rng.below(4) == 0 {
                epoch.saturating_sub(1)
            } else {
                epoch + 1
            };
            p = random_perms(&mut rng, epoch);
        }
        let command = random_command(&mut rng, &state, revs, &mut ops, n);
        if let Ok(applied) = state.apply(&command, &p) {
            check_invariants(&state, &applied.review);
            if applied.replayed {
                assert_eq!(applied.review, state);
            }
            state = applied.review;
        }
        for publisher in PEOPLE {
            let who_p = who(publisher);
            for index in 0..=3 {
                for exact in revs {
                    if let Ok(ok) = check_publishable(&state, index, exact, &p, &who_p) {
                        let current = state.current().unwrap();
                        assert_eq!(current.revision, *exact);
                        assert_eq!(ok.index, current.index);
                        assert_eq!(index, current.index);
                        assert_eq!(state.state(), RequestState::Open);
                        assert!(p.role(&who_p) == Some(Role::Maintain));
                        assert!(ok.approvals.len() as u32 >= p.required_approvals);
                        for a in &ok.approvals {
                            assert!(current.approvals.contains(a));
                            assert!(p.role(a) >= Some(Role::Review));
                            assert_ne!(Some(a), state.author());
                        }
                    }
                }
            }
        }
    }
    state
}

#[test]
fn random_sequences_preserve_invariants_and_are_deterministic() {
    let mut fx = Fixture::new();
    let revs = [
        fx.proposal(1, "a", None),
        fx.proposal(1, "b", Some(b"ctx")),
        fx.proposal(0, "a", None),
    ];
    let mut accepted = 0;
    let mut revised = 0;
    let mut approved = 0;
    for seed in 1..=400u64 {
        let first = run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), &revs);
        let again = run(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), &revs);
        assert_eq!(first, again);
        accepted += first.history().len();
        revised += first.revisions().len().saturating_sub(1);
        approved += first
            .revisions()
            .iter()
            .map(|v| v.approvals.len())
            .sum::<usize>();
    }
    // Guard against a generator that only produces refusals.
    assert!(accepted > 2000, "only {accepted} accepted commands");
    assert!(
        revised > 200 && approved > 300,
        "revised {revised} approved {approved}"
    );
}
