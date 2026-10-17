"""Focused offline regressions. No backend, mount, compiler or benchmark execution."""
import contextlib
import copy
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest


def module(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(f'{name}.py'))
    value = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(value)
    return value


audit, worker = module('audit'), module('worker')


class Pagination(unittest.TestCase):
    def setUp(self):
        self.p = {'delete': {'exit': 0}, 'rename': {'exit': 0}, 'afterDelete': {'status': 0},
                  'afterRename': {'status': 0}, 'traversal': {'exit': 0},
                  'remaining': {'exit': 0, 'stdout': ''}, 'mutationThreadFinished': True}

    def test_success_requires_completed_mutations_and_empty_scan(self):
        self.assertEqual(audit.pagination_verdict(self.p), 'VERIFIED')
        for key in ('delete', 'rename', 'remaining'):
            with self.subTest(key=key):
                p = copy.deepcopy(self.p); p[key]['exit'] = 1
                self.assertEqual(audit.pagination_verdict(p), 'INCONCLUSIVE')

    def test_leftovers_cookie_errors_and_failed_traversal_cannot_pass(self):
        p = copy.deepcopy(self.p); p['remaining']['stdout'] = '/mount/leftover\n'
        self.assertEqual(audit.pagination_verdict(p), 'NOT VERIFIED')
        p['remaining']['stdout'] = '\n'
        self.assertEqual(audit.pagination_verdict(p), 'NOT VERIFIED')
        for key in ('afterDelete', 'afterRename'):
            with self.subTest(key=key):
                p = copy.deepcopy(self.p); p[key]['status'] = 10003
                self.assertEqual(audit.pagination_verdict(p), 'NOT VERIFIED')
        p = copy.deepcopy(self.p); p['traversal']['exit'] = 1
        self.assertEqual(audit.pagination_verdict(p), 'NOT VERIFIED')


class LocalAccess(unittest.TestCase):
    def test_only_explicit_denial_passes(self):
        v = {'uid': 1500, 'directRead': {'ok': False, 'errno': 13}, 'nfsStatus': 13, 'nfsData': ''}
        self.assertEqual(audit.local_access_verdict(v, 501), 'VERIFIED')
        for status in (0, 5, 70, 10003):
            with self.subTest(status=status):
                self.assertEqual(audit.local_access_verdict(dict(v, nfsStatus=status), 501), 'INCONCLUSIVE')
        self.assertEqual(audit.local_access_verdict(dict(v, directRead={'ok': False, 'errno': 2}), 501), 'INCONCLUSIVE')

    def test_any_protected_bytes_fail_even_partial_or_wrong_content(self):
        for data in ('owner-only-secret\n', 'owner-', 'x'):
            with self.subTest(data=data):
                v = {'uid': 1500, 'directRead': {'ok': False, 'errno': 13}, 'nfsStatus': 0,
                     'nfsData': data, 'ownerBypass': data == 'owner-only-secret\n'}
                self.assertEqual(audit.local_access_verdict(v, 501), 'NOT VERIFIED')


class Costs(unittest.TestCase):
    def setUp(self):
        self.costs = []
        expected = audit.expected_fixture(32)
        body = ''.join(f'int value_{n}(void) {{ return {n}; }}\n' for n in range(32))
        for kind in ('ordinary-apfs', 'direct-clonefile', 'nfs'):
            self.costs.append({'kind': kind, 'entries': 32, 'baseFixture': copy.deepcopy(expected),
                'visibleFixture': copy.deepcopy(expected), 'errorCount': 0,
                'read': {'exit': 0, 'stdout': body},
                'compile': {'exit': 0, 'argv': ['clang', '-fsyntax-only', *[f'/mount/{v["name"]}' for v in expected]]},
                'objectCompile': {'exit': 0}, 'delete': {'exit': 0}})

    def test_bound_count_contents_compile_inputs_and_read_output(self):
        self.assertEqual(audit.cost_verdict(self.costs, 32), 'VERIFIED')
        cases = [lambda c: c['visibleFixture'].pop(),
                 lambda c: c['visibleFixture'][0].update(size=0, sha256='empty'),
                 lambda c: c['read'].update(stdout=''),
                 lambda c: c['compile']['argv'].pop()]
        for index, mutate in enumerate(cases):
            with self.subTest(case=index):
                costs = copy.deepcopy(self.costs); mutate(costs[-1])
                self.assertEqual(audit.cost_verdict(costs, 32), 'NOT VERIFIED')

    def test_historical_absent_visible_inventory_is_inconclusive(self):
        for c in self.costs:
            del c['baseFixture']; del c['visibleFixture']
        self.assertEqual(audit.cost_verdict(self.costs, 32), 'INCONCLUSIVE')

    def test_worker_rejects_missing_and_empty_visible_files_before_commands(self):
        with tempfile.TemporaryDirectory() as tmp:
            base, visible = Path(tmp) / 'base', Path(tmp) / 'view'
            base.mkdir(); visible.mkdir()
            (base / 'unit00000.c').write_text('int value_0(void) { return 0; }\n')
            for contents in (None, ''):
                with self.subTest(contents=contents):
                    if contents is not None: (visible / 'unit00000.c').write_text(contents)
                    with contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(ValueError, 'visible cost fixture'):
                        worker.cost(visible, None, 1, 'nfs', base)


if __name__ == '__main__':
    unittest.main()
