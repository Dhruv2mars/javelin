# Switching from Git and GitHub

This guide is for someone who uses Git and GitHub every day. It walks through a first week with
Javelin: install, a first repository on a node you run, daily work, review, a teammate, a
conflict, and a second device. Each command below ran in this order against a self-hosted node,
with a fresh `HOME` for each machine. The Cloudflare commands ran against a local `wrangler dev`.

## The map

| Git and GitHub | Javelin | What it is |
| --- | --- | --- |
| repository | repository | a `.javelin` directory in the project |
| `main` and its history | **World** | one line of numbered versions that never change |
| branch | **Layer** | isolated work over one World version, in a plain directory |
| commit | **checkpoint** | a snapshot of a Layer, taken by any `javelin` command that touches it, or by `javelin checkpoint` |
| staging area | none | a checkpoint takes the whole Layer directory |
| pull request | **Publish Request** | proposed work whose revisions never change; an approval counts for one revision |
| merge into `main` | **Publish** | the whole Layer becomes the next World version, or nothing does |
| rebase onto `main` | `javelin layer update` | moves the work onto the newest World as a new Layer |
| `git clone` | `javelin attach URL DIR` | a clone that follows the node |
| a remote, GitHub's servers | **node** | `javelin-node` on a machine you run, or a Worker in your Cloudflare account |
| `git push` | none | checkpoints upload as they are taken, and Publish runs on the node |
| `git pull`, `git fetch` | `javelin sync` | brings the node's new versions into the clone |
| collaborators, org members | `javelin member` | roles kept by a control plane you run |
| `.gitignore` | `.javelinignore` | the same idea, in the project root |

A Layer is a directory, so any editor, compiler or agent works in it without checking anything
out. There is no local history to rewrite, because the World only gets new versions.

## Install

Javelin will be on npm as `@dhruv2mars/javelin`. **No release is published yet**, so this
command finds no package today:

```sh
npm i -g @dhruv2mars/javelin      # or: bun add -g @dhruv2mars/javelin
```

Until then, build from source. You need an Entire login (`entire login`), a Rust toolchain and a
C compiler:

```sh
entire repo clone /et/dhruv2mars/javelin
cd javelin
cargo install --locked --path crates/javelin-cli
cargo install --locked --path crates/javelin-node
javelin --version
```

That installs the CLI, `javelin`, and the server that holds a repository, `javelin-node`.
[install.md](install.md) covers the npm and curl installers for when the release is out.

## 1. A control plane, and signing in

A control plane keeps accounts, devices and members, and hands out tokens. You run it
yourself. On your machine:

```sh
export JAVELIN_CONTROL_DIR=~/javelin-control
javelin control init                      # prints the admin token once: save it
javelin control serve 127.0.0.1:7979 &
export JAVELIN_CONTROL_URL=http://127.0.0.1:7979
javelin login                             # prints a code and waits
```

In a second terminal, with the same two variables set, approve that code with the admin token:

```sh
JAVELIN_ADMIN_TOKEN=jav_admin_... javelin control approve CODE
```

`javelin login` then finishes and saves this device's sign-in in `~/.javelin`. Signing in with
the admin token makes you the account `admin`.

## 2. A repository on a node

```sh
javelin repo create acme/app --operation create-app --visibility private --publish requests
```

It prints the repository's ID, which starts with `repo_`, and the line that serves it. Start the node and
attach a clone, the way you would `git clone`:

```sh
repo=repo_...                             # the ID repo create printed
javelin-node serve ~/javelin-control/nodes/$repo $repo 127.0.0.1:7878 &
javelin attach http://127.0.0.1:7878/$repo ~/app
```

`~/app` is now a clone of an empty World 0. Every write takes an `--operation` ID. If you lose
a command's result to a dropped connection or a crash, run it again with the same ID. You get the
first result back, and nothing changes twice.

### Or a node on Cloudflare

The node can also run as a Worker in your own Cloudflare account, on the Free plan. Deploy it
with the steps in [tools/node-cloudflare/README.md](../tools/node-cloudflare/README.md), which
also set its admin token. Then, signed in as in step 1, create the repository with the node's admin
token set and `JAVELIN_CONTROL_URL` unset, so the CLI opens the control plane directly:

```sh
export JAVELIN_NODE_ADMIN_TOKEN=...       # the token you gave the Worker
env -u JAVELIN_CONTROL_URL javelin repo create acme/app --operation create-app \
  --visibility private --publish requests --provider cloudflare --node-url https://NODE.workers.dev
repo=repo_...                             # the ID it printed
javelin attach https://NODE.workers.dev/$repo ~/app
```

Two things change in the rest of this guide on a Cloudflare node. Every attach URL is the
Worker's, `https://NODE.workers.dev/$repo`, including Cora's in steps 5 and 8. And only a
control plane that holds the node's admin token can change members or settings there, so run
the `member` and `repo settings` commands of step 5 the same way. An invite does not reach the
node by itself. Once the teammate has signed in with it, add them again. That `member add` says
nothing changed, and it still pushes the role to the node:

```sh
env -u JAVELIN_CONTROL_URL javelin member invite acme/app cora --role review
env -u JAVELIN_CONTROL_URL javelin repo settings acme/app --required-approvals 1
env -u JAVELIN_CONTROL_URL javelin member add acme/app cora --role review   # after cora's login --invite
```

## 3. Bring in your Git checkout

`javelin adopt` imports a project directory's files as one Publish. It never reads `.git`, and
it never writes to the project. It does not read `.gitignore` either. `.javelinignore` takes
only literal paths from the project root, such as `target/`, with no globs or negation, so
translate the rules you need first ([ignore rules](cli.md#ignore-rules)). The examples below use
a small Rust crate with `README.md` and `src/lib.rs`; change the paths to fit your project.

```sh
printf 'target/\n' > ~/code/app/.javelinignore
javelin adopt ~/code/app --repo ~/app --operation import-app
cd ~/app
javelin history                           # World 0, then World 1 with your files
```

Your Git history stays in Git. World 1 holds the files as they are now, without the commits that
led to them. A first import needs a World with no files yet.

## 4. Daily work in a Layer

```sh
javelin workspace create readme           # a Layer and its directory
ls -A .javelin/layers/readme              # your files, a plain directory
printf '# app\n\nGreets people by name.\n' > .javelin/layers/readme/README.md
javelin layer diff readme                 # captures the directory, then lists changed paths
javelin workspace publish readme --operation readme-1
javelin history                           # World 2
```

Point your editor or agent at `.javelin/layers/readme`. Each Layer is its own directory, so two
agents on two Layers never see each other's half-finished files. As a maintainer of a repository
that needs no approvals, `workspace publish` puts the Layer on the World directly. That finishes
the Layer and removes its directory. Start the next piece of work with a new `workspace create`.

## 5. Bring in a teammate

Invite Cora as a reviewer, and from now on require one approval before a Publish:

```sh
javelin member invite acme/app cora --role review   # prints a one-time code, valid 7 days
javelin repo settings acme/app --required-approvals 1
```

Send Cora the code and the repository ID. On her machine, with `JAVELIN_CONTROL_URL` set:

```sh
javelin login --invite CODE               # makes the account cora and signs this device in
repo=repo_...                             # the ID you were sent
javelin attach http://127.0.0.1:7878/$repo ~/app
```

Back on yours, `javelin member list acme/app` shows `admin maintain` and `cora review`. Roles
are `read`, `review` and `maintain`. `member add`, `member role` and `member remove` change them,
and the node applies a change on the next request, with no restart.

## 6. Requests and review

Cora works in a Layer and proposes it. A reviewer cannot publish directly, and `workspace
publish` tells her to open a request instead:

```sh
cd ~/app
javelin workspace create greeting
printf 'pub fn greet(name: &str) -> String {\n    format!("hello, {name}!")\n}\n' > .javelin/layers/greeting/src/lib.rs
printf 'Run app.\n' > .javelin/layers/greeting/USAGE.md
javelin request open greeting --layer greeting --operation cora-open-1
```

You review it:

```sh
javelin request list
javelin request show greeting             # revision 1: A "USAGE.md", M "src/lib.rs"
javelin request comment greeting --revision 1 --body 'Say what app does' --operation owner-comment-1
javelin request approve greeting --revision 1 --operation owner-approve-1
```

Cora reads the comment and revises:

```sh
javelin request show greeting
printf 'Run app to greet the world.\n' > .javelin/layers/greeting/USAGE.md
javelin request revise greeting --revision 1 --operation cora-revise-1
```

Revision 2 starts with no approvals, because yours was for revision 1's exact content. Approve
revision 2 and publish it:

```sh
javelin request approve greeting --revision 2 --operation owner-approve-2
javelin request publish greeting --revision 2 --operation owner-publish-2
javelin sync                              # bring World 3 into your clone
```

An author cannot approve their own request. At Cora's next `javelin sync`, her `greeting` Layer
is retired, since its work is now in the World.

## 7. A conflict, and `layer update`

Publish composes Layers that touched different paths. When two Layers change the same path,
the second Publish stops and reports the path. To see one, you and Cora each change the same
line of `src/lib.rs`. You:

```sh
javelin workspace create shout
printf 'pub fn greet(name: &str) -> String {\n    format!("HELLO, {name}!")\n}\n' > .javelin/layers/shout/src/lib.rs
javelin request open shout --layer shout --operation owner-open-shout
```

Cora:

```sh
javelin sync                              # World 3; her greeting Layer is retired
javelin workspace create hi
printf 'pub fn greet(name: &str) -> String {\n    format!("hi, {name}!")\n}\n' > .javelin/layers/hi/src/lib.rs
javelin request open hi --layer hi --operation cora-open-hi
javelin request approve shout --revision 1 --operation cora-approve-shout
```

You approve hers, publish yours, then try hers:

```sh
javelin request approve hi --revision 1 --operation owner-approve-hi
javelin request publish shout --revision 1 --operation owner-publish-shout
javelin request publish hi --revision 1 --operation owner-publish-hi   # exit 3: src/lib.rs conflicts
```

Nothing was published. Cora moves her work onto the new World, the way she would rebase:

```sh
javelin sync
javelin layer radar hi                    # which paths would conflict, and with what
javelin layer update hi                   # exit 3: the work is now in Layer hi.2, with markers
cat .javelin/layers/hi.2/src/lib.rs
```

The file holds both sides between `<<<<<<< Layer hi` and `>>>>>>> remote World 4`, with the
common base between `|||||||` and `=======`. She edits it to what she wants and revises. The
request follows the new Layer, and `request revise` refuses while a marker is left:

```sh
printf 'pub fn greet(name: &str) -> String {\n    format!("Hi, {name}!")\n}\n' > .javelin/layers/hi.2/src/lib.rs
javelin request revise hi --revision 1 --operation cora-revise-hi
```

You approve revision 2 and publish it:

```sh
javelin request approve hi --revision 2 --operation owner-approve-hi-2
javelin request publish hi --revision 2 --operation owner-publish-hi-2
```

## 8. A second device

Cora starts notes on her laptop. Each checkpoint uploads to the node, which lets another of her
devices take the Layer. She opens a request for it too:

```sh
javelin sync
javelin workspace create notes
printf -- '- started on the laptop\n' > .javelin/layers/notes/NOTES.md
javelin request open notes --layer notes --operation cora-open-notes
```

On her desktop, with `JAVELIN_CONTROL_URL` set, she signs in and attaches:

```sh
javelin login                             # prints a code; on the laptop: javelin control approve CODE
repo=repo_...
javelin attach http://127.0.0.1:7878/$repo ~/app
cd ~/app
javelin layer take notes --operation take-notes
```

`layer take` brings the Layer and its directory to the desktop, and only the desktop can write
to it from now on. Javelin refuses a later edit from the laptop with `layer_stale`, and the
message names the `javelin layer clone` that keeps that edit. She finishes on the desktop:

```sh
printf -- '- started on the laptop\n- finished on the desktop\n' > .javelin/layers/notes/NOTES.md
javelin request revise notes --revision 1 --layer notes --operation desk-revise-notes
```

You approve and publish it, and every clone syncs to the same World:

```sh
javelin request approve notes --revision 2 --operation owner-approve-notes
javelin request publish notes --revision 2 --operation owner-publish-notes
javelin sync                              # on each machine: World 6 everywhere
```

## What is different, and what is not there yet

Different on purpose:

- There are no branches to switch, nothing to stash and no detached HEAD. Each piece of work is
  its own Layer directory, and you can have many open at once.
- Publish is all or nothing. A conflict publishes nothing and lists the paths.
- History never changes. There is no force-push, rebase of published work, or amend.
- An approval counts for one revision. Any change asks for a new approval.
- Javelin never runs or reads Git. Keep Git for old history if you want it. Javelin does not
  touch `.git`.

Not there yet:

- **No release.** The npm package and the installers are built but not published; build from
  source.
- **No CI inside Javelin.** Nothing runs checks on a request yet, and how it will work is not
  decided.
- **Windows is experimental.** It builds, but no Windows machine has ever run it. macOS and
  Linux are the supported platforms.
- **A self-hosted node listens on loopback only.** `javelin-node serve` binds a loopback
  address. `javelin control serve` needs a TLS proxy in front of it, named with `--origin
  https://HOST`, before other machines can reach it. Clients use `https` for anything but
  `localhost`. In this guide every machine was a separate `HOME` on one computer.
- **The forge is read-only.** [javelin-codes.vercel.app](https://javelin-codes.vercel.app)
  shows files, history and diffs from a node URL. Requests, review, members and connecting a
  Cloudflare account from the browser are not built.
- An invited account has no passkey. It signs in only through an invite or another of its own
  devices.
- Attach URLs name the repository by its ID, `repo_...`, not by `acme/app`.
- `javelin history` in a clone lists only the Worlds that clone holds, one per attach or sync,
  not the node's whole history. `blame` in an attached clone has no origins from the node yet.

[cli.md](cli.md) is the full command reference. [TASKS.md](../TASKS.md) tracks what is being
built.
