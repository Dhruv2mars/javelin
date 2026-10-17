"""Read-only bounded inventory of this proof's private backing directories."""
import hashlib
import json
import os
from pathlib import Path
import stat
import sys

root = Path(sys.argv[1])
assert root.is_absolute() and root.resolve() == root and not root.is_symlink()
assert root.name.startswith("javelin-overlay-proof.")
assert (root / "owner").read_text().strip() == str(root)
trees = {}
for name in ["lower", "a-upper", "b-upper", "c-upper", "a-next-upper", "c-next-upper"]:
    tree = root / "backing" / name
    rows = []
    total_bytes = 0
    blocks = 0

    def visit(path):
        global total_bytes, blocks
        info = path.lstat()
        assert len(rows) < 64, "inventory entry budget exceeded"
        row = {"path": str(path.relative_to(tree)), "inode": info.st_ino,
               "device": info.st_dev, "mode": stat.S_IMODE(info.st_mode)}
        blocks += info.st_blocks
        if stat.S_ISLNK(info.st_mode):
            row.update(type="symlink", target=os.readlink(path))
        elif stat.S_ISDIR(info.st_mode):
            row.update(type="directory")
        elif stat.S_ISREG(info.st_mode):
            total_bytes += info.st_size
            assert total_bytes <= 256 * 1024, "inventory byte budget exceeded"
            row.update(type="file", bytes=info.st_size,
                       sha256=hashlib.sha256(path.read_bytes()).hexdigest())
        else:
            assert stat.S_ISCHR(info.st_mode), "unexpected special file"
            row.update(type="whiteout", major=os.major(info.st_rdev), minor=os.minor(info.st_rdev))
            assert info.st_rdev == 0, "unexpected device"
        row["xattrs"] = {key: os.getxattr(path, key, follow_symlinks=False).hex()
                         for key in os.listxattr(path, follow_symlinks=False)}
        rows.append(row)
        if stat.S_ISDIR(info.st_mode):
            for child in sorted(path.iterdir()):
                visit(child)

    visit(tree)
    trees[name] = {"entries": rows, "fileBytes": total_bytes, "allocatedBytes": blocks * 512}
print(json.dumps(trees, indent=2))
