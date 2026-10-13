#!/usr/bin/env bash
set -euo pipefail
source_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
prefix=${1:-"$HOME/.local"}
python3 - "$source_dir" "$prefix" "cloud-contacts" <<'PYINSTALL'
from pathlib import Path
import shutil,sys
source,prefix,product=Path(sys.argv[1]),Path(sys.argv[2]).expanduser().resolve(),sys.argv[3]
package=product.replace('-','_')
app=prefix/'share'/product
app.mkdir(parents=True,exist_ok=True)
for name in ['bin',package]:
 shutil.copytree(source/name,app/name,dirs_exist_ok=True,ignore=shutil.ignore_patterns('__pycache__','*.pyc'))
for name in ['README.md','LICENSE']:
 shutil.copyfile(source/name,app/name)
launcher=prefix/'bin'/product
launcher.parent.mkdir(parents=True,exist_ok=True)
# A Python launcher handles paths containing spaces without shell interpolation.
launcher.write_text('#!/usr/bin/env python3\nimport runpy,sys\nsys.path.insert(0, '+repr(str(app))+')\nrunpy.run_path('+repr(str(app/'bin'/product))+', run_name="__main__")\n')
launcher.chmod(0o755)
desktop=prefix/'share/applications'/('com.ferdousbhai.'+product.replace('-','')+'.desktop')
desktop.parent.mkdir(parents=True,exist_ok=True)
# Desktop Entry quoting is not shell quoting.
quoted=str(launcher).replace('\\','\\\\').replace('"','\\"').replace('`','\\`').replace('$','\\$')
desktop.write_text('[Desktop Entry]\nType=Application\nName='+product.replace('-',' ').title()+'\nExec="'+quoted+'" --app\nIcon=x-office-address-book\nTerminal=false\nCategories=Office;\n')
print('Installed '+str(launcher)+' and native app launcher. Existing account data is unchanged.')
PYINSTALL
