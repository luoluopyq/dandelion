#!/usr/bin/env python3
"""Make a release overlay from a tag; avoid installer/metadata version drift.
Usage: python3 scripts/prepare-release.py v0.2.0 --out path/to/release.json
"""
import argparse,json,pathlib,re
p=argparse.ArgumentParser();p.add_argument('version',nargs='?');p.add_argument('--out',required=True);p.add_argument('--updater',action='store_true');args=p.parse_args()
root=pathlib.Path(__file__).resolve().parents[1]
version=(args.version or json.loads((root/'apps/desktop/src-tauri/tauri.conf.json').read_text(encoding='utf-8'))['version']).removeprefix('v')
if not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?',version):raise SystemExit('Invalid semantic version')
parts=version.split('+',1)
if len(parts)>1 and any(not item for item in parts[1].split('.')):raise SystemExit('Invalid build metadata')
prerelease=parts[0].partition('-')[2]
if prerelease and any(not item or (item.isdigit() and len(item)>1 and item.startswith('0')) for item in prerelease.split('.')):raise SystemExit('Invalid prerelease identifiers')
root=pathlib.Path(__file__).resolve().parents[1]
# Keep Rust package info, the Tauri config and npm metadata aligned. This
# script runs in a release checkout; building an overlay alone would leave
# env!(CARGO_PKG_VERSION) stale and repeatedly offer the same update.
cargo=root/'Cargo.toml'
source=cargo.read_text(encoding='utf-8')
source,count=re.subn(r'(?m)^version = "[^"]+"$', 'version = "'+version+'"',source,count=1)
if count!=1:raise SystemExit('Workspace version not found')
source=re.sub(r'(dandelion-core = \{[^\n]*version = ")[^"]+("[^\n]*\})',lambda m:m[1]+version+m[2],source)
cargo.write_text(source,encoding='utf-8')
for filename in ['apps/desktop/src-tauri/tauri.conf.json','apps/desktop/package.json','apps/desktop/package-lock.json']:
    file=root/filename;data=json.loads(file.read_text(encoding='utf-8'));data['version']=version
    if filename.endswith('package-lock.json'):data.get('packages',{}).get('',{})['version']=version
    file.write_text(json.dumps(data,ensure_ascii=False,indent=2)+'\n',encoding='utf-8')
path=pathlib.Path(args.out);path.parent.mkdir(parents=True,exist_ok=True)
overlay={'version':version}
if args.updater:overlay['bundle']={'createUpdaterArtifacts':True}
path.write_text(json.dumps(overlay,indent=2)+'\n',encoding='utf-8')
print(path)
