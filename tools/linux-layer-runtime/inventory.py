"""Root-side read-only inventory of one Linux Layer runtime's hidden backing tree.

Lists caches, private uppers and staging leftovers with inodes, modes, sizes,
hashes, whiteouts and xattrs. Never follows symlinks and never writes."""
import hashlib
import json
import os
from pathlib import Path
import stat
import sys

PARENT = Path("/var/lib/javelin-layer-ci")
MAX_ENTRIES, MAX_BYTES = 4096, 16 * 1024 * 1024

root = Path(sys.argv[1])
assert len(sys.argv) == 2 and root.is_absolute() and not root.is_symlink()
assert root.resolve() == root and root.parent == PARENT and root.name.startswith("rt-")
assert (root / "owner").read_text().startswith("javelin-linux-layer-runtime v1\n")
hidden = root / "hidden"


def tree(base):
    rows, total, blocks = [], 0, 0

    def visit(path):
        nonlocal total, blocks
        info = path.lstat()
        assert len(rows) < MAX_ENTRIES, "inventory entry budget exceeded"
        blocks += info.st_blocks
        if path != base:
            row = {"path": str(path.relative_to(base)), "inode": info.st_ino,
                   "uid": info.st_uid, "mode": oct(stat.S_IMODE(info.st_mode))}
            if stat.S_ISLNK(info.st_mode):
                row.update(type="symlink", target=os.readlink(path))
            elif stat.S_ISDIR(info.st_mode):
                row.update(type="directory")
            elif stat.S_ISREG(info.st_mode):
                total += info.st_size
                assert total <= MAX_BYTES, "inventory byte budget exceeded"
                with open(path, "rb", opener=lambda p, f: os.open(p, f | os.O_NOFOLLOW)) as fh:
                    digest = hashlib.sha256(fh.read()).hexdigest()
                row.update(type="file", bytes=info.st_size, sha256=digest)
            elif stat.S_ISCHR(info.st_mode) and info.st_rdev == 0:
                row.update(type="whiteout")
            else:
                row.update(type="other")
            row["xattrs"] = {k: os.getxattr(path, k, follow_symlinks=False).hex()
                             for k in sorted(os.listxattr(path, follow_symlinks=False))}
            rows.append(row)
        if stat.S_ISDIR(info.st_mode):
            for child in sorted(path.iterdir()):
                visit(child)

    visit(base)
    return {"entries": rows, "fileBytes": total, "allocatedBytes": blocks * 512}


def names(relative):
    return sorted(p.name for p in (hidden / relative).iterdir())


print(json.dumps({
    "caches": {name: tree(hidden / "cache" / name) for name in names("cache")},
    "uppers": {name: tree(hidden / "views" / name / "upper") for name in names("views")
               if (hidden / "views" / name / "upper").is_dir()},
    "stages": names("stage"),
}, indent=1, sort_keys=True))
