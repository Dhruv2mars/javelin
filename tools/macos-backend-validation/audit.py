"""Offline integrity and semantic audit. No mount, backend execution or Git."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile

PHASES = ('source environment build fixture mount aliases pagination restart recovery local-access '
          'cost-32 cost-512 cost-2048 scale-ceiling watch-apfs watch-nfs capabilities huge-fixture '
          'truncate-controls truncate-nfs copy-up-stage source-after').split()


def digest(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def load(p):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result: raise ValueError(f'duplicate JSON key {key}')
            result[key] = value
        return result
    return json.loads(p.read_text(), object_pairs_hook=unique)


def seal(root):
    values = {str(p.relative_to(root)): digest(p) for p in root.rglob('*') if p.is_file()
              and p.name not in ('manifest.json', 'audit.json')}
    (root / 'manifest.json').write_text(json.dumps(values, indent=2) + '\n')


def pagination_verdict(p):
    operations = [p[k] for k in ('delete', 'rename', 'traversal', 'remaining')]
    if any(c.get('timeout') for c in operations) or not p['mutationThreadFinished']:
        return 'INCONCLUSIVE'
    if p['delete']['exit'] != 0 or p['rename']['exit'] != 0 or p['remaining']['exit'] != 0:
        return 'INCONCLUSIVE'
    bad = p['afterDelete']['status'] != 0 or p['afterRename']['status'] != 0 or \
        p['traversal']['exit'] != 0 or bool(p['remaining']['stdout'])
    return 'NOT VERIFIED' if bad else 'VERIFIED'


def local_access_verdict(v, owner_uid):
    if v['uid'] in (0, owner_uid) or v['directRead']['ok'] or v['directRead'].get('errno') not in (1, 13):
        return 'INCONCLUSIVE'
    # Any bytes from the protected file are a disclosure, including a partial read.
    if v['nfsData']:
        return 'NOT VERIFIED'
    return 'VERIFIED' if v['nfsStatus'] in (1, 13) else 'INCONCLUSIVE'


def expected_fixture(count):
    return [{'name': f'unit{n:05}.c', 'size': len(body), 'sha256': hashlib.sha256(body).hexdigest()}
            for n in range(count) for body in [f'int value_{n}(void) {{ return {n}; }}\n'.encode()]]


def cost_verdict(costs, count):
    if len(costs) != 3 or {v['kind'] for v in costs} != {'ordinary-apfs', 'direct-clonefile', 'nfs'}:
        return 'INCONCLUSIVE'
    expected = expected_fixture(count)
    expected_lines = sorted(f'int value_{n}(void) {{ return {n}; }}\n' for n in range(count))
    missing = False
    for c in costs:
        if c['entries'] != count or c.get('baseFixture') != expected:
            missing = True
            continue
        if 'visibleFixture' not in c:
            missing = True
            continue
        if c['visibleFixture'] != expected:
            return 'NOT VERIFIED'
        if any(c[k].get('timeout') for k in ('read', 'compile', 'objectCompile', 'delete')):
            return 'INCONCLUSIVE'
        if c['errorCount'] or sorted(c['read']['stdout'].splitlines(keepends=True)) != expected_lines:
            return 'NOT VERIFIED'
        if sorted(Path(p).name for p in c['compile']['argv'][2:]) != [v['name'] for v in expected]:
            return 'NOT VERIFIED'
    return 'INCONCLUSIVE' if missing else 'VERIFIED'


def audit(root, sha, run, producer_source=None):
    manifest = load(root / 'manifest.json')
    actual_files = {str(p.relative_to(root)) for p in root.rglob('*') if p.is_file()
                    and p.name not in ('manifest.json', 'audit.json')}
    if set(manifest) != actual_files: raise ValueError('missing or extra artifact file')
    for name, expected in manifest.items():
        p = root / name
        if p.is_symlink() or not p.resolve().is_relative_to(root.resolve()): raise ValueError('unsafe artifact path')
        if digest(p) != expected: raise ValueError(f'changed bytes: {name}')
    identity = load(root / 'identity.json')
    if identity['sha'] != sha or identity['runId'] != run: raise ValueError('external source/run identity mismatch')
    if identity['backend'] != '39a065b075f2a55688628eb7b3fd64f75b05334e': raise ValueError('wrong backend identity')
    pin = load(Path(__file__).with_name('backend-source.json'))
    for name, expected in pin.items():
        if digest(root / 'source' / name) != expected: raise ValueError(f'changed backend source: {name}')
    # Historical producers may be checked by a newer auditor without changing their bytes.
    source = Path(producer_source) if producer_source else Path(__file__).parent
    if not all((source / name).is_file() for name in ('audit.py', 'worker.py', 'runner.mjs', 'watch.c', 'backend-source.json')):
        raise ValueError('incomplete external producer source')
    for p in source.iterdir():
        if p.is_file() and p.suffix in ('.json', '.mjs', '.py', '.c'):
            if digest(root / 'source/tools/macos-backend-validation' / p.name) != digest(p):
                raise ValueError(f'proof source differs: {p.name}')
    phases = {}
    for p in root.glob('phase-*.json'):
        value = load(p); name = value['name']
        if name in phases or p.name != f'phase-{name}.json': raise ValueError('duplicate/mismatched phase')
        if value['state'] not in ('finished', 'inconclusive') or not value.get('finishedAt'): raise ValueError('unfinished phase')
        phases[name] = value
    if set(phases) != set(PHASES): raise ValueError('missing/extra phase')
    commands, started = {}, {}
    for line in (root / 'commands.jsonl').read_text().splitlines():
        c = json.loads(line)
        if c['state'] == 'started':
            if c['id'] in started: raise ValueError('duplicate command start')
            started[c['id']] = c
        elif c['state'] == 'finished':
            if c['id'] not in started or c['id'] in commands: raise ValueError('orphan/duplicate command finish')
            if c['argv'] != started[c['id']]['argv']: raise ValueError('command identity changed')
            if not c.get('finishedAt'): raise ValueError('unfinished command')
            commands[c['id']] = c
        else: raise ValueError('unknown command state')
    if set(started) != set(commands): raise ValueError('unfinished command groups')
    for c in [*started.values(), *commands.values()]:
        for stream in ('stdout', 'stderr'):
            name = c[stream]
            if name not in manifest or not (root / name).is_file():
                raise ValueError(f'missing referenced {stream}: {c["id"]}')
        if c['state'] == 'finished' and any(c[k] != started[c['id']][k] for k in ('stdout', 'stderr')):
            raise ValueError('command log identity changed')
    def contents(c, stream='stdout'):
        return (root / c[stream]).read_text(errors='replace')
    def command(ident): return commands[ident]
    def records(name):
        result = []
        for c in commands.values():
            if c['phase'] != name: continue
            for line in contents(c).splitlines():
                if line.startswith('{'):
                    try: result.append(json.loads(line))
                    except json.JSONDecodeError: pass
        return result
    def values(name, key): return [v[key] for v in records(name) if key in v]
    results = {}
    def outcome(name, verdict, detail):
        claim = phases[name].get('claimedVerdict')
        if claim is not None and claim != verdict: raise ValueError(f'false phase verdict: {name}')
        results[name] = {'verdict': verdict, 'detail': detail}
    for name in PHASES:
        cs = [c for c in commands.values() if c['phase'] == name and 'serve' not in c['argv']]
        if phases[name]['state'] != 'finished' or any(c.get('timedOut') for c in cs):
            outcome(name, 'INCONCLUSIVE', phases[name].get('error', phases[name].get('reason', 'command timeout')))
        else: outcome(name, 'INCONCLUSIVE', 'executed; capability-specific proof required')
    for label in ['backend-before', 'backend-after']:
        p = root / f'{label}.json'
        if p.exists() and load(p) != pin: raise ValueError('backend hash mismatch')
    if (root / 'backend-after.json').exists(): outcome('source-after', 'VERIFIED', 'all pinned backend files unchanged')
    aliases = values('aliases', 'aliases')
    if aliases:
        rows = aliases[-1]
        if len(rows) != 2: raise ValueError('case/normalization must be distinct')
        status = 'VERIFIED'
        for row in rows:
            control = row['control']
            if not control['ok'] or not control['value']['sameInode']:
                if status != 'NOT VERIFIED': status = 'INCONCLUSIVE'
                continue
            removed = row['remove']['exit'] == 0
            bypass = removed and row['aliasRead']['exit'] == 0
            if row['whiteoutBypass'] != bypass: raise ValueError('false whiteout result')
            if bypass: status = 'NOT VERIFIED'
            if not removed or row['exact']['exit'] == 0: status = 'NOT VERIFIED'
        outcome('aliases', status, rows)
    pages = values('pagination', 'pagination')
    if pages:
        p = pages[-1]
        crossed = p['first']['status'] == p['second']['status'] == 0 and not p['first']['eof'] and bool(p['second']['entries'])
        if not crossed: raise ValueError('pagination claim without multiple successful READDIRPLUS responses')
        verdict = pagination_verdict(p)
        outcome('pagination', verdict, {'firstEntries': len(p['first']['entries']), 'secondEntries': len(p['second']['entries']),
                'afterDeleteStatus': p['afterDelete']['status'], 'afterRenameStatus': p['afterRename']['status'],
                'traversalExit': p['traversal']['exit'], 'remaining': p['remaining'], 'mutations': p['mutations']})
    ready = [v for v in records('restart') if v.get('ready')]
    observed = [v for v in records('restart') if v.get('sameMountAfterRestart')]
    if ready and observed:
        detail = phases['restart']['detail']; observation = command(detail['observation']['id'])
        if detail['oldServer']['signal'] != 'SIGKILL': raise ValueError('server was not SIGKILLed')
        if detail['restarted']['pid'] == detail['oldServer']['pid']: raise ValueError('server PID not replaced')
        for c in commands.values():
            if '/sbin/umount' in c['argv'] and c['startedAt'] < observation['finishedAt']:
                raise ValueError('unmounted before held FD observation completed')
        seen = observed[-1]
        okay = ready[-1]['fsyncCompleted'] and all(seen[k]['ok'] for k in
                 ('heldWriteFd', 'heldBaseFd', 'postRestartWrite', 'heldDirectory', 'newOpen', 'root'))
        okay = okay and seen['heldBaseFd']['value']['contents'].endswith('unread-tail\n')
        outcome('restart', 'VERIFIED' if okay else 'NOT VERIFIED', {'ready': ready[-1], 'observed': seen, 'handles': values('restart', 'handles')})
    recovery = values('recovery', 'recovery')
    if recovery:
        value = recovery[-1]
        expected = 'completed-fsynced-write\n'
        preserved = value['contents'].startswith(expected)
        if value['preservedAcknowledged'] != preserved: raise ValueError('false recovery result')
        verdict = 'VERIFIED' if preserved and ready and ready[-1]['fsyncCompleted'] else 'NOT VERIFIED'
        outcome('recovery', verdict, value)
    local = values('local-access', 'secondUser')
    if local:
        v = local[-1]
        outcome('local-access', local_access_verdict(v, identity['uid']), v)
    for n in [32, 512, 2048]:
        name = f'cost-{n}'; costs = values(name, 'cost')
        if costs:
            for c in costs:
                error_count = sum(c[k]['exit'] != 0 for k in ['read', 'compile', 'objectCompile', 'delete'])
                if error_count != c['errorCount']: raise ValueError('false cost error count')
            outcome(name, cost_verdict(costs, n), costs)
        fixtures = values(name, 'costFixture')
        if any(v['baseFixture'] == expected_fixture(n) and v['visibleFixture'] != v['baseFixture'] for v in fixtures):
            outcome(name, 'NOT VERIFIED', fixtures)
    for name in ['watch-apfs', 'watch-nfs']:
        watches = [v for v in records(name) if 'fseventsStarted' in v]
        if watches:
            v = watches[-1]
            passed = v['mutationBytes'] == 6 and v['fsync'] == 0 and v['fseventsStarted'] and v['fseventsCount'] > 0 and v['kqueueCount'] > 0
            outcome(name, 'VERIFIED' if passed else 'NOT VERIFIED', v)
    caps = values('capabilities', 'capabilities')
    if caps: outcome('capabilities', 'VERIFIED' if all(caps[-1][k]['ok'] for k in ['hardLink', 'setxattr', 'getxattr']) else 'NOT VERIFIED', caps[-1])
    truncates = values('truncate-nfs', 'truncate')
    if truncates: outcome('truncate-nfs', 'VERIFIED' if truncates[-1]['newSize'] == 0 else 'NOT VERIFIED', truncates[-1])
    stages = values('copy-up-stage', 'staged'); huge = values('huge-fixture', 'hugeFixture')
    if stages and huge:
        hashes = [v['sha256'] for v in records('huge-fixture') if 'hugeFixture' in v]
        matches = [v for v in stages[-1] if v['size'] == 1073741824 and v['sha256'] == hashes[-1]]
        outcome('copy-up-stage', 'VERIFIED' if matches else 'NOT VERIFIED', stages[-1])
    cleanup = load(root / 'cleanup.json'); ledger = load(root / 'ledger.json')
    mount_command = command(cleanup['mountTable']); mount_text = contents(mount_command)
    group_command = command(cleanup['processTable'])
    owned = {g['pid'] for g in ledger['groups'] if g['id'] != group_command['id']}
    remaining = [line for line in contents(group_command).splitlines() if len(line.split()) > 1 and int(line.split()[1]) in owned]
    account_ok = ledger['account'] is None
    if ledger['account']:
        a = command(cleanup['accountCommand'])
        account_ok = a['exit'] != 0 and any(s in contents(a) + contents(a, 'stderr') for s in ('eDSRecordNotFound', 'DS Error: -14136', 'Invalid Path'))
    verified = mount_command['exit'] == group_command['exit'] == 0 and not remaining and account_ok and \
        not any(f' on {p} (' in mount_text for p in ledger['mounts']) and \
        command(cleanup['portCommand'])['exit'] == 1 and command(cleanup['rootCommand'])['exit'] == 0
    if cleanup['verified'] != verified: raise ValueError('false cleanup claim')
    required = ['aliases', 'pagination', 'restart', 'recovery', 'local-access', 'watch-nfs', 'truncate-nfs']
    verdict = 'NOT VERIFIED' if any(results[n]['verdict'] == 'NOT VERIFIED' for n in required) else 'INCONCLUSIVE'
    return {'schema': 1, 'identity': identity, 'integrity': 'VERIFIED', 'cleanup': 'VERIFIED' if verified else 'NOT VERIFIED',
            'suitability': verdict, 'phases': results,
            'recommendation': 'Harden or replace the NFS layer before native binding. No product gate is accepted.'}


def selftest():
    here = Path(__file__).parent; repo = here.parents[1]
    backend_repo = Path(os.environ.get('SUITABILITY_BACKEND_ROOT', str(repo / 'backend-source')))
    with tempfile.TemporaryDirectory(prefix='javelin-audit-test-') as tmp:
        baseline = Path(tmp) / 'baseline'; baseline.mkdir()
        for name in load(here / 'backend-source.json'):
            dest = baseline / 'source' / name; dest.parent.mkdir(parents=True, exist_ok=True); shutil.copyfile(backend_repo / name, dest)
        for p in here.iterdir():
            if p.is_file():
                dest = baseline / 'source/tools/macos-backend-validation' / p.name
                dest.parent.mkdir(parents=True, exist_ok=True); shutil.copyfile(p, dest)
        def put(root, name, value): (root / name).write_text(json.dumps(value))
        put(baseline, 'identity.json', {'sha': 'test-sha', 'runId': 'test-run', 'backend': '39a065b075f2a55688628eb7b3fd64f75b05334e', 'uid': 501})
        for name in PHASES: put(baseline, f'phase-{name}.json', {'name': name, 'state': 'inconclusive', 'finishedAt': 'z'})
        put(baseline, 'ledger.json', {'groups': [], 'mounts': ['/owned/mount'], 'work': '/owned', 'account': None})
        commands = []
        for name, code in [('mount', 0), ('ps', 0), ('port', 1), ('root', 0), ('build', 0)]:
            (baseline / f'{name}.stdout').write_text(''); (baseline / f'{name}.stderr').write_text('')
            c = {'id': name, 'argv': [name], 'phase': 'build' if name == 'build' else 'cleanup', 'stdout': f'{name}.stdout', 'stderr': f'{name}.stderr', 'state': 'started'}
            commands += [dict(c), dict(c, state='finished', exit=code, finishedAt='z')]
        (baseline / 'commands.jsonl').write_text(''.join(json.dumps(c) + '\n' for c in commands))
        put(baseline, 'cleanup.json', {'mountTable': 'mount', 'processTable': 'ps', 'portCommand': 'port', 'rootCommand': 'root', 'verified': True})
        seal(baseline); assert audit(baseline, 'test-sha', 'test-run')['suitability'] == 'INCONCLUSIVE'
        mutations = {
            'changed-bytes': lambda r: (r / 'mount.stdout').write_text('changed'),
            'missing-log': lambda r: (r / 'mount.stdout').unlink(),
            'missing-build-stdout-resealed': lambda r: (r / 'build.stdout').unlink(),
            'missing-build-stderr-resealed': lambda r: (r / 'build.stderr').unlink(),
            'missing-phase': lambda r: (r / 'phase-aliases.json').unlink(),
            'unfinished-phase': lambda r: put(r, 'phase-aliases.json', {'name': 'aliases', 'state': 'started'}),
            'duplicate-phase': lambda r: shutil.copyfile(r / 'phase-aliases.json', r / 'phase-duplicate.json'),
            'false-success-resealed': lambda r: put(r, 'phase-aliases.json', {'name': 'aliases', 'state': 'inconclusive', 'finishedAt': 'z', 'claimedVerdict': 'VERIFIED'}),
            'false-cleanup-resealed': lambda r: (r / 'mount.stdout').write_text('127.0.0.1:/ on /owned/mount (nfs)\n'),
            'wrong-identity': lambda r: put(r, 'identity.json', {'sha': 'wrong', 'runId': 'test-run'}),
        }
        def false_whiteout(r):
            c = {'id': 'alias', 'argv': ['probe'], 'phase': 'aliases', 'stdout': 'alias.stdout', 'stderr': 'alias.stderr', 'state': 'started'}
            with (r / 'commands.jsonl').open('a') as f:
                f.write(json.dumps(c) + '\n' + json.dumps(dict(c, state='finished', exit=0, finishedAt='z')) + '\n')
            row = {'control': {'ok': True, 'value': {'sameInode': True}}, 'remove': {'exit': 0},
                   'aliasRead': {'exit': 0}, 'whiteoutBypass': False, 'exact': {'exit': 1}}
            (r / 'alias.stdout').write_text(json.dumps({'aliases': [row, row]}) + '\n'); (r / 'alias.stderr').write_text('')
        mutations['false-alias-result-resealed'] = false_whiteout
        for name, mutate in mutations.items():
            dest = Path(tmp) / name; shutil.copytree(baseline, dest); mutate(dest)
            if name not in ('changed-bytes', 'missing-log'): seal(dest)
            try: audit(dest, 'test-sha', 'test-run')
            except (ValueError, KeyError, FileNotFoundError): print(f'rejected {name}')
            else: raise AssertionError(f'audit accepted {name}')
        print('offline audit negative controls passed')


if __name__ == '__main__':
    if sys.argv[1:] == ['--selftest']: selftest()
    else:
        root, sha, run, *producer = sys.argv[1:]
        result = audit(Path(root), sha, run, producer[0] if producer else None)
        print(json.dumps(result, indent=2))
