import json,pathlib,shutil,subprocess,sys,tempfile,unittest
ROOT=pathlib.Path(__file__).resolve().parents[2]
class ReleaseVersionTests(unittest.TestCase):
 def fixture(self):
  directory=tempfile.TemporaryDirectory();self.addCleanup(directory.cleanup);root=pathlib.Path(directory.name);(root/'scripts').mkdir();shutil.copy(ROOT/'scripts/prepare-release.py',root/'scripts/prepare-release.py')
  (root/'Cargo.toml').write_text('[workspace.package]\nversion = "0.1.0"\n[workspace.dependencies]\ndandelion-core = { path = "crates/dandelion-core", version = "0.1.0" }\n')
  for name in ['apps/desktop/src-tauri/tauri.conf.json','apps/desktop/package.json','apps/desktop/package-lock.json']:
   file=root/name;file.parent.mkdir(parents=True,exist_ok=True);file.write_text(json.dumps({'version':'0.1.0','packages':{'':{'version':'0.1.0'}}}))
  return root
 def test_release_versions_stay_consistent(self):
  root=self.fixture();subprocess.run([sys.executable,str(root/'scripts/prepare-release.py'),'v0.2.0-beta.1','--out',str(root/'overlay.json')],check=True,capture_output=True)
  cargo=(root/'Cargo.toml').read_text();self.assertEqual(cargo.count('version = "0.2.0-beta.1"'),2)
  for file in ['apps/desktop/src-tauri/tauri.conf.json','apps/desktop/package.json','apps/desktop/package-lock.json','overlay.json']:
   self.assertEqual(json.loads((root/file).read_text())['version'],'0.2.0-beta.1')
  self.assertEqual(json.loads((root/'apps/desktop/package-lock.json').read_text())['packages']['']['version'],'0.2.0-beta.1')
 def test_invalid_versions_do_not_modify_checkout(self):
  root=self.fixture();before=(root/'Cargo.toml').read_text()
  for value in ['1.2.3; echo bad','1.02.3','1.2.3-beta..1','1.2.3-01']:
   r=subprocess.run([sys.executable,str(root/'scripts/prepare-release.py'),value,'--out',str(root/'overlay.json')],capture_output=True);self.assertNotEqual(r.returncode,0)
  self.assertEqual((root/'Cargo.toml').read_text(),before)
if __name__=='__main__':unittest.main()
