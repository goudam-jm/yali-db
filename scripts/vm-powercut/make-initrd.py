"""Build a minimal RAM-only guest. Only /dev/vda is ever formatted, inside QEMU."""
import gzip
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

OUT = Path('/app/guest'); OUT.mkdir()
version = sorted(Path('/lib/modules').iterdir())[-1].name
shutil.copyfile(f'/boot/vmlinuz-{version}', OUT / 'kernel')
with tempfile.TemporaryDirectory() as temp:
    root = Path(temp)
    for name in ('bin', 'sbin', 'proc', 'sys', 'dev', 'data', 'tmp', 'run'):
        (root / name).mkdir(parents=True, exist_ok=True)
    def copy(source, dest=None):
        target = root / (dest or source).lstrip('/')
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        target.chmod(Path(source).stat().st_mode)
    def binary(source, dest=None):
        copy(source, dest)
        dependencies = subprocess.run(['ldd', source], text=True, capture_output=True).stdout
        for lib in re.findall(r'(/[^\s()]+)', dependencies):
            if Path(lib).is_file(): copy(lib)
    binary('/bin/busybox')
    for name in ('sh', 'mount', 'mkdir', 'stty', 'cat', 'uname', 'sync', 'sleep'):
        (root / 'bin' / name).symlink_to('busybox')
    binary('/sbin/modprobe')
    binary('/sbin/mkfs.ext4')
    binary('/sbin/mkfs.xfs')
    binary('/app/target/release/examples/storage_probe', '/storage_probe')
    for module in ('virtio_mmio', 'virtio_pci', 'virtio_blk', 'crc32c_generic', 'ext4', 'xfs'):
        lines = subprocess.check_output(['modprobe', '-S', version, '--show-depends', module], text=True)
        for line in lines.splitlines():
            if line.startswith('insmod '): copy(line.split()[1])
    for source in (Path('/lib/modules') / version).glob('modules.*'):
        if source.is_file(): copy(str(source))
    copy('/app/scripts/vm-powercut/guest-init.sh', '/init')
    (root / 'init').chmod(0o755)
    listing = subprocess.check_output(['find', '.', '-print0'], cwd=root)
    archive = subprocess.check_output(['cpio', '--null', '-o', '--format=newc', '--quiet'], input=listing, cwd=root)
    with gzip.open(OUT / 'initrd.gz', 'wb', compresslevel=1) as stream: stream.write(archive)
(OUT / 'kernel-version').write_text(version + '\n')
