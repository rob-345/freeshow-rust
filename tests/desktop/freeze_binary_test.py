"""Corruption controls for the production GUI binary isolation used by CI."""
import importlib.util
import json
from pathlib import Path
import tempfile
import sys
sys.dont_write_bytecode = True
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('freeze', Path(__file__).resolve().parents[2] / 'scripts/migration/freeze-desktop-binary.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class FreezeBinary(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='freeshow-freeze-control-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binary = self.root / 'freeshow-desktop'
        self.binary.write_bytes(b'Controlled binary bytes\x00\xff')
        self.resources = self.root / 'resources'
        self.resources.mkdir()
        (self.resources / 'index.html').write_text('Controlled resource')
        self.destination = self.root / 'isolated/target/debug'
        self.receipt = self.root / 'receipt.json'
        self.log = self.root / 'build.jsonl'
        self.rows = [
            {'reason': 'compiler-artifact', 'target': {'name': 'freeshow-desktop', 'kind': ['bin']}, 'features': ['custom-protocol'], 'profile': {'test': False}, 'executable': str(self.binary)},
            {'reason': 'compiler-artifact', 'target': {'name': 'freeshow_browse'}, 'features': []},
            {'reason': 'compiler-artifact', 'target': {'name': 'freeshow_ai_status'}, 'features': []},
            {'reason': 'build-finished', 'success': True},
        ]

    def run_freeze(self):
        self.log.write_text('\n'.join(json.dumps(row) for row in self.rows) + '\n')
        return module.freeze(self.log, self.binary, self.resources, self.destination, self.receipt)

    def test_frozen_bytes_and_native_resource_layout_survive_later_cargo_replacement(self):
        result = self.run_freeze()
        original = self.binary.read_bytes()
        self.binary.write_bytes(b'Later test harness binary')
        self.assertEqual((self.destination / self.binary.name).read_bytes(), original)
        self.assertEqual((self.destination / 'remotes/index.html').read_text(), 'Controlled resource')
        self.assertTrue((self.destination / '.cargo-lock').is_file())
        self.assertEqual(result['binary']['sha256'], module.digest(self.destination / self.binary.name))
        self.assertFalse(result['compilerProvenanceAttested'])
        with self.assertRaises(AssertionError):
            self.run_freeze()
        self.assertEqual((self.destination / self.binary.name).read_bytes(), original)

    def test_feature_unification_and_incomplete_or_foreign_builds_reject_before_copy(self):
        import copy
        original = copy.deepcopy(self.rows)
        variants = [lambda r: r[1]['features'].append('test-harness'), lambda r: r[2]['features'].append('test-harness'), lambda r: r[0]['profile'].update(test=True), lambda r: r[-1].update(success=False), lambda r: r[0].update(executable=str(self.resources / 'index.html'))]
        for mutate in variants:
            with self.subTest(mutation=variants.index(mutate)):
                self.rows = copy.deepcopy(original)
                mutate(self.rows)
                with self.assertRaises(AssertionError):
                    self.run_freeze()
                self.assertFalse(self.destination.exists())
                self.assertFalse(self.receipt.exists())

    def test_source_replacement_during_copy_rejects_and_removes_owned_partial_output(self):
        copy2 = module.shutil.copy2
        def changed(source, target):
            copy2(source, target)
            source.write_bytes(b'Concurrent Cargo replacement')
        with patch.object(module.shutil, 'copy2', changed):
            with self.assertRaises(AssertionError):
                self.run_freeze()
        self.assertFalse(self.destination.exists())
        self.assertFalse(self.receipt.exists())

    def test_unrecorded_resource_added_during_copy_rejects_incomplete_manifest(self):
        copytree = module.shutil.copytree
        def changed(source, target):
            copytree(source, target)
            (target / 'unrecorded').write_text('Added while copying')
        with patch.object(module.shutil, 'copytree', changed):
            with self.assertRaises(AssertionError):
                self.run_freeze()
        self.assertFalse(self.destination.exists())
        self.assertFalse(self.receipt.exists())

    def test_symlinked_resource_never_enters_the_frozen_tree(self):
        (self.resources / 'external').symlink_to(self.binary)
        with self.assertRaises(AssertionError):
            self.run_freeze()
        self.assertFalse(self.destination.exists())
        self.assertFalse(self.receipt.exists())


if __name__ == '__main__':
    unittest.main()
